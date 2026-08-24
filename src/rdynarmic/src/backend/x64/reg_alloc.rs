use rxbyak::{xmmword_ptr, RSP};
use rxbyak::{CodeAssembler, Reg, RegExp};

use crate::backend::x64::constant_pool::ConstantPool;

use crate::backend::x64::abi;
use crate::backend::x64::block_of_code::STACK_LAYOUT_RSP_OFFSET;
use crate::backend::x64::hostloc::*;
use crate::backend::x64::oparg::OpArg;
use crate::backend::x64::stack_layout::{StackLayout, SPILL_COUNT};
use crate::ir::cond::Cond;
use crate::ir::inst::MAX_ARGS;
use crate::ir::opcode::Opcode;
use crate::ir::types::Type;
use crate::ir::value::{InstRef, Value};

// ---------------------------------------------------------------------------
// Flat indexing for the hostloc_info array
// ---------------------------------------------------------------------------

const NUM_GPRS: usize = 16;
const NUM_XMMS: usize = 16;
const NON_SPILL_COUNT: usize = NUM_GPRS + NUM_XMMS; // 32
const SPILL_SLOTS: usize = SPILL_COUNT;
const TOTAL_HOSTLOC_COUNT: usize = NON_SPILL_COUNT + SPILL_SLOTS;

fn hostloc_to_index(loc: HostLoc) -> usize {
    match loc {
        HostLoc::Gpr(i) => i as usize,
        HostLoc::Xmm(i) => NUM_GPRS + i as usize,
        HostLoc::Spill(i) => NON_SPILL_COUNT + i as usize,
    }
}

fn index_to_hostloc(index: usize) -> HostLoc {
    if index < NUM_GPRS {
        HostLoc::Gpr(index as u8)
    } else if index < NON_SPILL_COUNT {
        HostLoc::Xmm((index - NUM_GPRS) as u8)
    } else {
        HostLoc::Spill((index - NON_SPILL_COUNT) as u8)
    }
}

fn is_valueless_type(ty: Type) -> bool {
    matches!(ty, Type::Table)
}

// ---------------------------------------------------------------------------
// Per-location tracking
// ---------------------------------------------------------------------------

/// Tracks the state of a single host location (register or spill slot).
#[derive(Debug, Clone)]
struct HostLocInfo {
    /// How many times this location has been locked this scope.
    is_being_used_count: usize,
    /// Whether this location is a scratch register (write-locked).
    is_scratch: bool,
    /// Whether this location's value is on its last use.
    is_set_last_use: bool,

    /// Current argument references (from GetArgumentInfo).
    current_references: usize,
    /// Accumulated uses counted so far.
    accumulated_uses: usize,
    /// Total expected uses (from IR use_count).
    total_uses: usize,

    /// IR values currently stored in this location.
    values: Vec<InstRef>,
    /// Maximum bit width of values stored here.
    max_bit_width: usize,
}

impl HostLocInfo {
    fn new() -> Self {
        Self {
            is_being_used_count: 0,
            is_scratch: false,
            is_set_last_use: false,
            current_references: 0,
            accumulated_uses: 0,
            total_uses: 0,
            values: Vec::new(),
            max_bit_width: 0,
        }
    }

    fn is_locked(&self) -> bool {
        self.is_being_used_count > 0
    }

    fn is_empty(&self) -> bool {
        self.is_being_used_count == 0 && self.values.is_empty()
    }

    fn is_last_use(&self) -> bool {
        self.is_being_used_count == 0
            && self.current_references == 1
            && self.accumulated_uses + 1 == self.total_uses
    }

    fn set_last_use(&mut self) {
        assert!(
            self.is_last_use(),
            "set_last_use requires upstream IsLastUse()"
        );
        self.is_set_last_use = true;
    }

    fn read_lock(&mut self) {
        assert!(!self.is_scratch, "ReadLock on scratch host location");
        self.is_being_used_count += 1;
    }

    fn write_lock(&mut self) {
        assert!(
            self.is_being_used_count == 0,
            "WriteLock requires unlocked host location"
        );
        self.is_being_used_count += 1;
        self.is_scratch = true;
    }

    fn add_arg_reference(
        &mut self,
        current_inst: Option<(u64, InstRef, Opcode)>,
        referenced_inst: InstRef,
        arg_index: usize,
    ) {
        self.current_references += 1;
        assert!(
            self.accumulated_uses + self.current_references <= self.total_uses,
            "Too many arg references: current_inst={current_inst:?} referenced_inst={referenced_inst:?} arg_index={arg_index} accumulated={} current={} total={}",
            self.accumulated_uses,
            self.current_references,
            self.total_uses,
        );
    }

    fn release_one(&mut self) {
        assert!(
            self.is_being_used_count > 0,
            "ReleaseOne on unlocked host location"
        );
        self.is_being_used_count -= 1;
        self.is_scratch = false;

        if self.current_references == 0 {
            return;
        }

        self.accumulated_uses += 1;
        self.current_references -= 1;

        if self.current_references == 0 {
            self.release_all();
        }
    }

    fn release_all(&mut self) {
        self.accumulated_uses += self.current_references;
        self.current_references = 0;
        self.is_set_last_use = false;

        if self.total_uses == self.accumulated_uses {
            self.values.clear();
            self.accumulated_uses = 0;
            self.total_uses = 0;
            self.max_bit_width = 0;
        }

        self.is_being_used_count = 0;
        self.is_scratch = false;
    }

    fn contains_value(&self, inst: InstRef) -> bool {
        self.values.contains(&inst)
    }

    fn get_max_bit_width(&self) -> usize {
        self.max_bit_width
    }

    fn add_value(&mut self, inst: InstRef, bit_width: usize, total_uses: usize) {
        if self.is_set_last_use {
            self.is_set_last_use = false;
            self.values.clear();
        }
        self.values.push(inst);
        self.total_uses += total_uses;
        if bit_width > self.max_bit_width {
            self.max_bit_width = bit_width;
        }
    }
}

// ---------------------------------------------------------------------------
// Argument — wraps a Value extracted from an IR instruction
// ---------------------------------------------------------------------------

/// An argument extracted from an IR instruction for register allocation.
pub struct Argument {
    /// Whether this argument has been allocated to a host location.
    pub allocated: bool,
    /// The IR value this argument represents.
    pub value: Value,
}

impl Argument {
    fn new() -> Self {
        Self {
            allocated: false,
            value: Value::Void,
        }
    }

    pub fn get_type(&self) -> Type {
        self.value.get_type()
    }

    pub fn is_immediate(&self) -> bool {
        self.value.is_immediate()
    }

    pub fn is_void(&self) -> bool {
        matches!(self.value, Value::Void)
    }

    pub fn fits_in_immediate_u32(&self) -> bool {
        if let Some(v) = self.get_immediate_u64_opt() {
            v <= u32::MAX as u64
        } else {
            false
        }
    }

    pub fn fits_in_immediate_s32(&self) -> bool {
        if let Some(v) = self.get_immediate_u64_opt().map(|v| v as i64) {
            v >= i32::MIN as i64 && v <= i32::MAX as i64
        } else {
            false
        }
    }

    pub fn get_immediate_u1(&self) -> bool {
        match self.value {
            Value::ImmU1(v) => v,
            _ => panic!("Expected ImmU1"),
        }
    }

    pub fn get_immediate_u8(&self) -> u8 {
        match self.value {
            Value::ImmU8(v) => v,
            _ => panic!("Expected ImmU8"),
        }
    }

    pub fn get_immediate_u16(&self) -> u16 {
        match self.value {
            Value::ImmU16(v) => v,
            _ => panic!("Expected ImmU16"),
        }
    }

    pub fn get_immediate_u32(&self) -> u32 {
        match self.value {
            Value::ImmU32(v) => v,
            _ => panic!("Expected ImmU32"),
        }
    }

    pub fn get_immediate_u64(&self) -> u64 {
        self.value.get_imm_as_u64()
    }

    pub fn get_immediate_s32(&self) -> u64 {
        assert!(self.fits_in_immediate_s32());
        self.value.get_imm_as_u64()
    }

    pub fn get_immediate_cond(&self) -> Cond {
        match self.value {
            Value::ImmCond(c) => c,
            _ => panic!("Expected ImmCond"),
        }
    }

    fn get_immediate_u64_opt(&self) -> Option<u64> {
        if self.is_immediate() && !self.is_void() {
            Some(self.value.get_imm_as_u64())
        } else {
            None
        }
    }

    /// Check if this argument's value is currently in a GPR.
    pub fn is_in_gpr(&self, reg_alloc: &RegAlloc) -> bool {
        if let Value::Inst(inst_ref) = self.value {
            if let Some(loc) = reg_alloc.value_location(inst_ref) {
                return loc.is_gpr();
            }
        }
        false
    }

    /// Check if this argument's value is currently in an XMM register.
    pub fn is_in_xmm(&self, reg_alloc: &RegAlloc) -> bool {
        if let Value::Inst(inst_ref) = self.value {
            if let Some(loc) = reg_alloc.value_location(inst_ref) {
                return loc.is_xmm();
            }
        }
        false
    }

    /// Check if this argument's value is currently in a spill slot.
    pub fn is_in_memory(&self, reg_alloc: &RegAlloc) -> bool {
        if let Value::Inst(inst_ref) = self.value {
            if let Some(loc) = reg_alloc.value_location(inst_ref) {
                return loc.is_spill();
            }
        }
        false
    }
}

/// Array of arguments for an instruction (up to MAX_ARGS).
pub type ArgumentInfo = [Argument; MAX_ARGS];

// ---------------------------------------------------------------------------
// RegAlloc — the register allocator
// ---------------------------------------------------------------------------

/// Register allocator that maps IR values to x86-64 host registers.
///
/// Tracks which IR values live in which host locations (GPRs, XMMs, spill slots),
/// handles spilling when pressure is high, and emits move/exchange instructions
/// as needed.
pub struct RegAlloc<'a> {
    /// The code assembler for emitting spill/reload/move instructions.
    pub asm: &'a mut CodeAssembler,
    /// Constant pool for XMM immediate loading (RIP-relative).
    /// Matches upstream: `code.Const(code.xword, imm_value)`.
    /// None in tests where no constant pool is available.
    pub constant_pool: Option<&'a mut ConstantPool>,
    /// Preferred GPR allocation order.
    gpr_order: Vec<HostLoc>,
    /// Preferred XMM allocation order.
    xmm_order: Vec<HostLoc>,
    /// Per-location state tracking.
    hostloc_info: Vec<HostLocInfo>,
    /// Extra stack space reserved for host calls.
    reserved_stack_space: usize,
    /// Block instruction data for looking up use counts and types.
    /// (inst_ref → (use_count, return_type_bit_width))
    inst_info: Vec<(u32, usize)>,
    /// Current IR instruction being emitted, used only for one-shot diagnostics.
    current_inst: Option<(u64, InstRef, Opcode)>,
}

impl<'a> RegAlloc<'a> {
    /// Create a new register allocator.
    ///
    /// `inst_info` should contain (use_count, return_type_bit_width) for each
    /// instruction in the block, indexed by InstRef.
    pub fn new(
        asm: &'a mut CodeAssembler,
        gpr_order: Vec<HostLoc>,
        xmm_order: Vec<HostLoc>,
        inst_info: Vec<(u32, usize)>,
    ) -> Self {
        Self {
            asm,
            constant_pool: None,
            gpr_order,
            xmm_order,
            hostloc_info: (0..TOTAL_HOSTLOC_COUNT)
                .map(|_| HostLocInfo::new())
                .collect(),
            reserved_stack_space: 0,
            inst_info,
            current_inst: None,
        }
    }

    /// Create with default GPR/XMM ordering (from ANY_GPR/ANY_XMM).
    pub fn new_default(asm: &'a mut CodeAssembler, inst_info: Vec<(u32, usize)>) -> Self {
        Self::new(asm, ANY_GPR.to_vec(), ANY_XMM.to_vec(), inst_info)
    }

    pub fn set_current_inst_for_diagnostics(
        &mut self,
        block_pc: u64,
        inst_ref: InstRef,
        opcode: Opcode,
    ) {
        self.current_inst = Some((block_pc, inst_ref, opcode));
    }

    // -------------------------------------------------------------------
    // Argument info
    // -------------------------------------------------------------------

    /// Extract argument info for an instruction.
    ///
    /// Returns an array of Arguments, one per IR argument. Each non-immediate
    /// argument's host location gets its reference count bumped.
    pub fn get_argument_info(
        &mut self,
        _inst_ref: InstRef,
        args: &[Value],
        num_args: usize,
    ) -> ArgumentInfo {
        let mut ret: ArgumentInfo = std::array::from_fn(|_| Argument::new());
        for i in 0..num_args {
            let arg = args[i];
            ret[i].value = arg;
            if let Value::Inst(ref_inst) = arg {
                if is_valueless_type(arg.get_type()) {
                    continue;
                }
                let loc = self.value_location(ref_inst).unwrap_or_else(|| {
                    panic!(
                        "argument must already be defined: inst_ref={:?} arg_index={} arg={:?}",
                        _inst_ref, i, arg
                    )
                });
                let current_inst = self.current_inst;
                self.loc_info_mut(loc)
                    .add_arg_reference(current_inst, ref_inst, i);
            }
        }
        ret
    }

    /// Upstream parity: pseudo-op handlers register argument references even
    /// when the pseudo-op result has already been defined inline by the
    /// producing instruction.
    pub fn register_pseudo_operation(
        &mut self,
        inst_ref: InstRef,
        args: &[Value],
        num_args: usize,
    ) {
        assert!(
            self.is_value_live(inst_ref)
                || (inst_ref.0 as usize) >= self.inst_info.len()
                || self.inst_info[inst_ref.0 as usize].0 == 0,
            "pseudo-op must already be live or unused"
        );

        for (arg_index, arg) in args.iter().take(num_args).enumerate() {
            if let Value::Inst(ref_inst) = *arg {
                if is_valueless_type(arg.get_type()) {
                    continue;
                }
                if let Some(loc) = self.value_location(ref_inst) {
                    let current_inst = self.current_inst;
                    self.loc_info_mut(loc)
                        .add_arg_reference(current_inst, ref_inst, arg_index);
                }
            }
        }
    }

    /// Check if a value is still live (present in some host location).
    pub fn is_value_live(&self, inst_ref: InstRef) -> bool {
        self.value_location(inst_ref).is_some()
    }

    // -------------------------------------------------------------------
    // Use — read-only access to a value
    // -------------------------------------------------------------------

    /// Use a value in a GPR (read-only). Returns the x86-64 register.
    pub fn use_gpr(&mut self, arg: &mut Argument) -> Reg {
        assert!(!arg.allocated, "Argument already allocated");
        arg.allocated = true;
        let loc = self.use_impl(arg.value, &self.gpr_order.clone());
        loc.to_reg64()
    }

    pub fn use_op_arg(&mut self, arg: &mut Argument) -> OpArg {
        OpArg::from(self.use_gpr(arg))
    }

    /// Use a value in an XMM register (read-only). Returns the XMM register.
    pub fn use_xmm(&mut self, arg: &mut Argument) -> Reg {
        assert!(!arg.allocated, "Argument already allocated");
        arg.allocated = true;
        let loc = self.use_impl(arg.value, &self.xmm_order.clone());
        loc.to_xmm()
    }

    /// Use a value in a specific host location (read-only).
    pub fn use_loc(&mut self, arg: &mut Argument, host_loc: HostLoc) {
        assert!(!arg.allocated, "Argument already allocated");
        arg.allocated = true;
        self.use_impl(arg.value, &[host_loc]);
    }

    // -------------------------------------------------------------------
    // UseScratch — read+write access (value is consumed)
    // -------------------------------------------------------------------

    /// Use a value as scratch in a GPR (read+write, consumes the value).
    pub fn use_scratch_gpr(&mut self, arg: &mut Argument) -> Reg {
        assert!(!arg.allocated, "Argument already allocated");
        arg.allocated = true;
        let loc = self.use_scratch_impl(arg.value, &self.gpr_order.clone());
        loc.to_reg64()
    }

    /// Use a value as scratch in an XMM (read+write, consumes the value).
    pub fn use_scratch_xmm(&mut self, arg: &mut Argument) -> Reg {
        assert!(!arg.allocated, "Argument already allocated");
        arg.allocated = true;
        let loc = self.use_scratch_impl(arg.value, &self.xmm_order.clone());
        loc.to_xmm()
    }

    /// Use a value as scratch in a specific host location.
    pub fn use_scratch(&mut self, arg: &mut Argument, host_loc: HostLoc) {
        assert!(!arg.allocated, "Argument already allocated");
        arg.allocated = true;
        self.use_scratch_impl(arg.value, &[host_loc]);
    }

    // -------------------------------------------------------------------
    // Scratch — allocate a fresh register
    // -------------------------------------------------------------------

    /// Allocate a scratch GPR (no value, write-locked).
    pub fn scratch_gpr(&mut self) -> Reg {
        let loc = self.scratch_impl(&self.gpr_order.clone());
        loc.to_reg64()
    }

    /// Allocate a specific GPR as scratch.
    pub fn scratch_gpr_at(&mut self, desired: HostLoc) -> Reg {
        let loc = self.scratch_impl(&[desired]);
        loc.to_reg64()
    }

    /// Allocate a scratch XMM register.
    pub fn scratch_xmm(&mut self) -> Reg {
        let loc = self.scratch_impl(&self.xmm_order.clone());
        loc.to_xmm()
    }

    /// Allocate a specific XMM as scratch.
    pub fn scratch_xmm_at(&mut self, desired: HostLoc) -> Reg {
        let loc = self.scratch_impl(&[desired]);
        loc.to_xmm()
    }

    // -------------------------------------------------------------------
    // DefineValue — bind an IR instruction's result to a host location
    // -------------------------------------------------------------------

    /// Define an IR instruction's result as living in the given register.
    pub fn define_value(&mut self, inst_ref: InstRef, reg: Reg) {
        let loc = reg_to_hostloc(reg);
        self.define_value_impl(inst_ref, loc);
    }

    /// Define an IR instruction's result from an argument (copy elision).
    pub fn define_value_from_arg(&mut self, inst_ref: InstRef, arg: &Argument) {
        if arg.value.is_immediate() {
            // Load immediate into a scratch GPR and define there
            let loc = self.scratch_impl(&self.gpr_order.clone());
            self.define_value_impl(inst_ref, loc);
            self.load_immediate(arg.value, loc);
        } else if let Value::Inst(use_ref) = arg.value {
            let loc = self
                .value_location(use_ref)
                .expect("use_inst must already be defined");
            self.define_value_impl(inst_ref, loc);
        }
    }

    /// Release a register (mark as no longer locked by current scope).
    pub fn release(&mut self, reg: Reg) {
        let loc = reg_to_hostloc(reg);
        self.loc_info_mut(loc).release_one();
    }

    // -------------------------------------------------------------------
    // HostCall — set up for calling a host function
    // -------------------------------------------------------------------

    /// Prepare for a host function call.
    ///
    /// - `result_def`: If Some, the return value (in RAX) is defined for this inst.
    /// - `args`: Up to 4 arguments placed in ABI parameter registers.
    ///
    /// All caller-saved registers that aren't arguments or the result are
    /// spilled/scratched.
    pub fn host_call(&mut self, result_def: Option<InstRef>, args: &mut [Option<&mut Argument>]) {
        let args_hostloc = [
            abi::ABI_PARAMS[0],
            abi::ABI_PARAMS[1],
            abi::ABI_PARAMS[2],
            abi::ABI_PARAMS[3],
        ];

        // Scratch the return register
        self.scratch_impl(&[abi::ABI_RETURN]);
        if let Some(inst_ref) = result_def {
            self.define_value_impl(inst_ref, abi::ABI_RETURN);
        }

        // Place arguments in ABI registers
        for (i, arg_opt) in args.iter_mut().enumerate() {
            if i >= 4 {
                break;
            }
            if let Some(arg) = arg_opt {
                if !arg.is_void() {
                    self.use_scratch(arg, args_hostloc[i]);

                    // Zero-extend small types (LLVM ABI requirement)
                    let reg = args_hostloc[i].to_reg64();
                    match arg.get_type() {
                        Type::U8 => {
                            let r32 = Reg::gpr32(reg.get_idx());
                            // For idx 4..7 we must use `new_ext8` to emit the
                            // REX prefix. `gpr8(4..7)` without REX encodes as
                            // AH/CH/DH/BH (high-byte registers) instead of
                            // vcvt.u32.f32 whose helper expects fbits in RSI,
                            // but the `movzx esi, sil` was wrongly encoded as
                            // `movzx esi, dh` → fbits ended up as whatever DH
                            // happened to contain, triggering saturation to
                            // 0xFFFFFFFF and cascading the divergence.
                            let idx = reg.get_idx();
                            let r8 = if (4..8).contains(&idx) {
                                Reg::new_ext8(idx)
                            } else {
                                Reg::gpr8(idx)
                            };
                            let _ = self.asm.movzx(r32, r8);
                        }
                        Type::U16 => {
                            let r32 = Reg::gpr32(reg.get_idx());
                            let r16 = Reg::gpr16(reg.get_idx());
                            let _ = self.asm.movzx(r32, r16);
                        }
                        Type::U32 => {
                            let r32 = Reg::gpr32(reg.get_idx());
                            let _ = self.asm.mov(r32, r32);
                        }
                        _ => {}
                    }
                }
            }
        }

        // Scratch unused ABI param registers
        for (i, arg_opt) in args.iter().enumerate() {
            if i >= 4 {
                break;
            }
            if arg_opt.is_none() {
                self.scratch_impl(&[args_hostloc[i]]);
            }
        }

        // Scratch all other caller-saved registers
        for &loc in abi::CALLER_SAVE_GPRS {
            if loc == abi::ABI_RETURN {
                continue;
            }
            if args_hostloc.contains(&loc) {
                continue;
            }
            if !self.loc_info(loc).is_locked() {
                self.scratch_impl(&[loc]);
            }
        }
        for &loc in abi::CALLER_SAVE_XMMS {
            if !self.loc_info(loc).is_locked() {
                self.scratch_impl(&[loc]);
            }
        }
    }

    // -------------------------------------------------------------------
    // Stack space management
    // -------------------------------------------------------------------

    /// Reserve additional stack space for host calls.
    ///
    /// The size is rounded up to a multiple of 16 so RSP stays 16-byte
    /// aligned. The backend keeps RSP 16-aligned inside a block so that
    /// `movaps`/`movdqa` to `[rsp + 16*k]` (used when spilling XMMs and when
    /// building host-call argument frames, e.g. VectorTableLookup) do not
    /// `#GP`. Upstream dynarmic always allocs multiples of 16 (e.g.
    /// `(table_size + 2) * 16`); a caller passing a non-multiple
    /// (`size_of::<TableLookupFrame>() == 136`) would otherwise misalign RSP
    /// by 8 and fault the aligned SSE moves. `release_stack_space` rounds
    /// identically so the paired add/sub stay balanced and the
    /// `reserved_stack_space` accounting (used by `spill_address`) matches.
    pub fn alloc_stack_space(&mut self, size: usize) {
        let size = (size + 15) & !15;
        self.reserved_stack_space += size;
        let _ = self.asm.sub(rxbyak::RSP, size as i32);
    }

    /// Release previously reserved stack space (rounded up to 16, matching
    /// `alloc_stack_space`).
    pub fn release_stack_space(&mut self, size: usize) {
        let size = (size + 15) & !15;
        self.reserved_stack_space -= size;
        let _ = self.asm.add(rxbyak::RSP, size as i32);
    }

    // -------------------------------------------------------------------
    // End of allocation scope
    // -------------------------------------------------------------------

    /// Release all locks at the end of processing an instruction.
    /// Must be called after each instruction's emission is complete.
    pub fn end_of_alloc_scope(&mut self) {
        for info in &mut self.hostloc_info {
            info.release_all();
        }
    }

    /// Assert that no values remain live (called at end of block).
    pub fn assert_no_more_uses(&self) {
        for (i, info) in self.hostloc_info.iter().enumerate() {
            assert!(
                info.is_empty(),
                "HostLoc {:?} still contains values at end of block",
                index_to_hostloc(i)
            );
        }
    }

    // -------------------------------------------------------------------
    // Internal: location lookup
    // -------------------------------------------------------------------

    /// Find which host location contains the given IR value.
    pub fn value_location(&self, inst_ref: InstRef) -> Option<HostLoc> {
        for (i, info) in self.hostloc_info.iter().enumerate() {
            if info.contains_value(inst_ref) {
                return Some(index_to_hostloc(i));
            }
        }
        None
    }

    fn loc_info(&self, loc: HostLoc) -> &HostLocInfo {
        &self.hostloc_info[hostloc_to_index(loc)]
    }

    fn loc_info_mut(&mut self, loc: HostLoc) -> &mut HostLocInfo {
        &mut self.hostloc_info[hostloc_to_index(loc)]
    }

    // -------------------------------------------------------------------
    // Internal: core allocation logic
    // -------------------------------------------------------------------

    fn use_impl(&mut self, use_value: Value, desired_locations: &[HostLoc]) -> HostLoc {
        if use_value.is_immediate() {
            let scratch = self.scratch_impl(desired_locations);
            return self.load_immediate(use_value, scratch);
        }

        let use_ref = match use_value {
            Value::Inst(r) => r,
            _ => panic!("use_impl on non-Inst non-immediate value"),
        };

        let current_location = self.value_location(use_ref).unwrap_or_else(|| {
            panic!(
                "Value must already be defined: use_ref={:?} desired_locations={:?}",
                use_ref, desired_locations
            )
        });
        let max_bit_width = self.loc_info(current_location).get_max_bit_width();

        // Can we use the value where it already is?
        if desired_locations.contains(&current_location) {
            self.loc_info_mut(current_location).read_lock();
            return current_location;
        }

        // If the current location is locked, we must copy
        if self.loc_info(current_location).is_locked() {
            return self.use_scratch_impl(use_value, desired_locations);
        }

        // Move the value to a desired location
        let dest = self.select_a_register(desired_locations);
        if max_bit_width > dest.bit_width() {
            return self.use_scratch_impl(use_value, desired_locations);
        }

        // Can we exchange? Only GPR↔GPR
        if can_exchange(dest, current_location) {
            self.exchange(dest, current_location);
        } else {
            self.move_out_of_the_way(dest);
            self.move_value(dest, current_location);
        }
        self.loc_info_mut(dest).read_lock();
        dest
    }

    fn use_scratch_impl(&mut self, use_value: Value, desired_locations: &[HostLoc]) -> HostLoc {
        if use_value.is_immediate() {
            let scratch = self.scratch_impl(desired_locations);
            return self.load_immediate(use_value, scratch);
        }

        let use_ref = match use_value {
            Value::Inst(r) => r,
            _ => panic!("use_scratch_impl on non-Inst non-immediate value"),
        };

        let current_location = self.value_location(use_ref).unwrap_or_else(|| {
            panic!(
                "Value must already be defined: use_ref={:?} desired_locations={:?}",
                use_ref, desired_locations
            )
        });
        let bit_width = self.get_value_bit_width(use_ref);

        // Can we reuse in place?
        if desired_locations.contains(&current_location)
            && !self.loc_info(current_location).is_locked()
        {
            if !self.loc_info(current_location).is_last_use() {
                self.move_out_of_the_way(current_location);
            } else {
                self.loc_info_mut(current_location).set_last_use();
            }
            self.loc_info_mut(current_location).write_lock();
            return current_location;
        }

        // Copy to a new scratch location
        let dest = self.select_a_register(desired_locations);
        self.move_out_of_the_way(dest);
        self.copy_to_scratch(bit_width, dest, current_location);
        self.loc_info_mut(dest).write_lock();
        dest
    }

    fn scratch_impl(&mut self, desired_locations: &[HostLoc]) -> HostLoc {
        let location = self.select_a_register(desired_locations);
        self.move_out_of_the_way(location);
        self.loc_info_mut(location).write_lock();
        location
    }

    /// Check if a value has already been defined (by a pseudo-op handler).
    pub fn is_value_defined(&self, inst_ref: InstRef) -> bool {
        self.value_location(inst_ref).is_some()
    }

    fn define_value_impl(&mut self, inst_ref: InstRef, host_loc: HostLoc) {
        assert!(
            self.value_location(inst_ref).is_none(),
            "inst_ref {:?} has already been defined",
            inst_ref
        );
        let (use_count, bit_width) = if (inst_ref.0 as usize) < self.inst_info.len() {
            self.inst_info[inst_ref.0 as usize]
        } else {
            (1, 64) // fallback
        };
        self.loc_info_mut(host_loc)
            .add_value(inst_ref, bit_width, use_count as usize);
    }

    // -------------------------------------------------------------------
    // Internal: register selection
    // -------------------------------------------------------------------

    /// Select the best available register from the desired locations.
    /// Prefers unlocked, empty registers.
    fn select_a_register(&self, desired_locations: &[HostLoc]) -> HostLoc {
        // First pass: find unlocked, empty registers
        for &loc in desired_locations {
            if !self.loc_info(loc).is_locked() && self.loc_info(loc).is_empty() {
                return loc;
            }
        }
        // Second pass: find any unlocked register
        for &loc in desired_locations {
            if !self.loc_info(loc).is_locked() {
                return loc;
            }
        }
        panic!("All candidate registers have already been allocated");
    }

    // -------------------------------------------------------------------
    // Internal: immediate loading
    // -------------------------------------------------------------------

    fn load_immediate(&mut self, imm: Value, host_loc: HostLoc) -> HostLoc {
        assert!(imm.is_immediate(), "load_immediate called on non-immediate");

        if host_loc.is_gpr() {
            let reg = host_loc.to_reg64();
            let imm_value = imm.get_imm_as_u64();
            if imm_value == 0 {
                let r32 = Reg::gpr32(reg.get_idx());
                let _ = self.asm.xor_(r32, r32);
            } else {
                let _ = self.asm.mov(reg, imm_value as i64);
            }
            return host_loc;
        }

        if host_loc.is_xmm() {
            let reg = host_loc.to_xmm();
            let imm_value = imm.get_imm_as_u64();
            if imm_value == 0 {
                let _ = self.asm.xorps(reg, reg);
            } else {
                // Load from constant pool via RIP-relative addressing.
                // Matches upstream: `MAYBE_AVX(movaps, reg, code.Const(code.xword, imm_value))`
                // The pool returns a RegExp with rip_addr() — the encoder computes
                // the correct displacement at emit time. No instruction size guessing.
                // Panics if pool is full (matching upstream ASSERT).
                let pool = self
                    .constant_pool
                    .as_mut()
                    .expect("Constant pool required for non-zero XMM immediates");
                let addr = pool.get_constant(imm_value, 0);
                let _ = self.asm.movaps(reg, xmmword_ptr(addr));
            }
            return host_loc;
        }

        panic!("Cannot load immediate into spill slot directly");
    }

    // -------------------------------------------------------------------
    // Internal: move / exchange / spill
    // -------------------------------------------------------------------

    fn move_value(&mut self, to: HostLoc, from: HostLoc) {
        let bit_width = self.loc_info(from).get_max_bit_width();

        assert!(self.loc_info(to).is_empty(), "Destination must be empty");
        assert!(
            !self.loc_info(from).is_locked(),
            "Source must not be locked"
        );
        assert!(
            bit_width <= to.bit_width(),
            "Value too wide for destination"
        );

        if self.loc_info(from).is_empty() {
            return;
        }

        self.emit_move(bit_width, to, from);

        // Transfer state
        let from_info = std::mem::replace(
            &mut self.hostloc_info[hostloc_to_index(from)],
            HostLocInfo::new(),
        );
        self.hostloc_info[hostloc_to_index(to)] = from_info;
    }

    fn copy_to_scratch(&mut self, bit_width: usize, to: HostLoc, from: HostLoc) {
        assert!(self.loc_info(to).is_empty(), "Destination must be empty");
        assert!(!self.loc_info(from).is_empty(), "Source must not be empty");
        self.emit_move(bit_width, to, from);
    }

    fn exchange(&mut self, a: HostLoc, b: HostLoc) {
        assert!(!self.loc_info(a).is_locked() && !self.loc_info(b).is_locked());

        if self.loc_info(a).is_empty() {
            self.move_value(a, b);
            return;
        }
        if self.loc_info(b).is_empty() {
            self.move_value(b, a);
            return;
        }

        self.emit_exchange(a, b);

        let idx_a = hostloc_to_index(a);
        let idx_b = hostloc_to_index(b);
        self.hostloc_info.swap(idx_a, idx_b);
    }

    fn move_out_of_the_way(&mut self, reg: HostLoc) {
        assert!(
            !self.loc_info(reg).is_locked(),
            "Cannot move locked register"
        );
        if !self.loc_info(reg).is_empty() {
            self.spill_register(reg);
        }
    }

    fn spill_register(&mut self, loc: HostLoc) {
        assert!(loc.is_register(), "Only registers can be spilled");
        assert!(!self.loc_info(loc).is_empty(), "Nothing to spill");
        assert!(
            !self.loc_info(loc).is_locked(),
            "Cannot spill locked register"
        );

        let new_loc = self.find_free_spill();
        self.move_value(new_loc, loc);
    }

    fn find_free_spill(&self) -> HostLoc {
        for i in 0..SPILL_SLOTS {
            let loc = HostLoc::Spill(i as u8);
            if self.loc_info(loc).is_empty() {
                return loc;
            }
        }
        panic!("All spill locations are full");
    }

    // -------------------------------------------------------------------
    // Internal: code emission helpers
    // -------------------------------------------------------------------

    fn emit_move(&mut self, bit_width: usize, to: HostLoc, from: HostLoc) {
        match (to, from) {
            // GPR → GPR
            (HostLoc::Gpr(_), HostLoc::Gpr(_)) => {
                if bit_width == 64 {
                    let _ = self.asm.mov(to.to_reg64(), from.to_reg64());
                } else {
                    let to32 = Reg::gpr32(to.gpr_index());
                    let from32 = Reg::gpr32(from.gpr_index());
                    let _ = self.asm.mov(to32, from32);
                }
            }
            // XMM → XMM
            (HostLoc::Xmm(_), HostLoc::Xmm(_)) => {
                let _ = self.asm.movaps(to.to_xmm(), from.to_xmm());
            }
            // GPR → XMM
            (HostLoc::Xmm(_), HostLoc::Gpr(_)) => {
                // Upstream asserts `bit_width != 128` here, then release
                // builds fall through to the 32-bit `movd` path for any
                // non-64 width. Keep that behavior while logging the bad IR
                // width once for diagnosis.
                if bit_width > 64 {
                    use std::sync::atomic::{AtomicBool, Ordering};
                    static WARNED: AtomicBool = AtomicBool::new(false);
                    if !WARNED.swap(true, Ordering::Relaxed) {
                        if let Some((pc, inst_ref, opcode)) = self.current_inst {
                            eprintln!(
                                "[reg_alloc] WARN GPR→XMM bit_width={} > 64 pc=0x{:016X} inst#{} opcode={:?} from={:?} to={:?}; truncating (one-shot warn)",
                                bit_width, pc, inst_ref.0, opcode, from, to
                            );
                        } else {
                            eprintln!(
                                "[reg_alloc] WARN GPR→XMM bit_width={} > 64 from={:?} to={:?}; truncating (one-shot warn)",
                                bit_width, from, to
                            );
                        }
                    }
                    let _ = self.asm.movd(to.to_xmm(), Reg::gpr32(from.gpr_index()));
                } else if bit_width == 64 {
                    let _ = self.asm.movq(to.to_xmm(), from.to_reg64());
                } else {
                    let _ = self.asm.movd(to.to_xmm(), Reg::gpr32(from.gpr_index()));
                }
            }
            // XMM → GPR
            (HostLoc::Gpr(_), HostLoc::Xmm(_)) => {
                if bit_width > 64 {
                    use std::sync::atomic::{AtomicBool, Ordering};
                    static WARNED: AtomicBool = AtomicBool::new(false);
                    if !WARNED.swap(true, Ordering::Relaxed) {
                        if let Some((pc, inst_ref, opcode)) = self.current_inst {
                            eprintln!(
                                "[reg_alloc] WARN XMM→GPR bit_width={} > 64 pc=0x{:016X} inst#{} opcode={:?} from={:?} to={:?}; truncating (one-shot warn)",
                                bit_width, pc, inst_ref.0, opcode, from, to
                            );
                        } else {
                            eprintln!(
                                "[reg_alloc] WARN XMM→GPR bit_width={} > 64 from={:?} to={:?}; truncating (one-shot warn)",
                                bit_width, from, to
                            );
                        }
                    }
                    let _ = self.asm.movd(Reg::gpr32(to.gpr_index()), from.to_xmm());
                } else if bit_width == 64 {
                    let _ = self.asm.movq(to.to_reg64(), from.to_xmm());
                } else {
                    let _ = self.asm.movd(Reg::gpr32(to.gpr_index()), from.to_xmm());
                }
            }
            // Spill → XMM
            (HostLoc::Xmm(_), HostLoc::Spill(_)) => {
                let addr = self.spill_address(from);
                match bit_width {
                    128 => {
                        let _ = self.asm.movaps(to.to_xmm(), addr);
                    }
                    64 => {
                        let _ = self.asm.movsd(to.to_xmm(), addr);
                    }
                    _ => {
                        let _ = self.asm.movss(to.to_xmm(), addr);
                    }
                }
            }
            // XMM → Spill
            (HostLoc::Spill(_), HostLoc::Xmm(_)) => {
                let addr = self.spill_address(to);
                match bit_width {
                    128 => {
                        let _ = self.asm.movaps(addr, from.to_xmm());
                    }
                    64 => {
                        let _ = self.asm.movsd(addr, from.to_xmm());
                    }
                    _ => {
                        let _ = self.asm.movss(addr, from.to_xmm());
                    }
                }
            }
            // Spill → GPR
            (HostLoc::Gpr(_), HostLoc::Spill(_)) => {
                let addr = self.spill_address(from);
                if bit_width == 64 {
                    let _ = self.asm.mov(to.to_reg64(), addr);
                } else {
                    let _ = self.asm.mov(Reg::gpr32(to.gpr_index()), addr);
                }
            }
            // GPR → Spill
            (HostLoc::Spill(_), HostLoc::Gpr(_)) => {
                let addr = self.spill_address(to);
                if bit_width == 64 {
                    let _ = self.asm.mov(addr, from.to_reg64());
                } else {
                    let _ = self.asm.mov(addr, Reg::gpr32(from.gpr_index()));
                }
            }
            _ => panic!("Invalid emit_move: {:?} → {:?}", from, to),
        }
    }

    fn emit_exchange(&mut self, a: HostLoc, b: HostLoc) {
        match (a, b) {
            (HostLoc::Gpr(_), HostLoc::Gpr(_)) => {
                let _ = self.asm.xchg(a.to_reg64(), b.to_reg64());
            }
            _ => panic!("Exchange only supported for GPR↔GPR"),
        }
    }

    fn spill_address(&self, loc: HostLoc) -> rxbyak::Address {
        let i = match loc {
            HostLoc::Spill(i) => i as usize,
            _ => panic!("spill_address called on non-spill"),
        };
        assert!(i < SPILL_SLOTS, "Spill index out of range");
        let offset =
            self.reserved_stack_space + STACK_LAYOUT_RSP_OFFSET + StackLayout::spill_offset(i);
        xmmword_ptr(RegExp::from(RSP) + offset as i32)
    }

    fn get_value_bit_width(&self, inst_ref: InstRef) -> usize {
        if (inst_ref.0 as usize) < self.inst_info.len() {
            self.inst_info[inst_ref.0 as usize].1
        } else {
            64 // fallback
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert an rxbyak Reg to a HostLoc.
fn reg_to_hostloc(reg: Reg) -> HostLoc {
    let idx = reg.get_idx();
    let bit = reg.get_bit();
    if bit >= 128 {
        // XMM register
        HostLoc::Xmm(idx)
    } else {
        // GPR
        HostLoc::Gpr(idx)
    }
}

/// Check if two locations can be exchanged (only GPR↔GPR).
fn can_exchange(a: HostLoc, b: HostLoc) -> bool {
    a.is_gpr() && b.is_gpr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hostloc_indexing_round_trip() {
        for i in 0..TOTAL_HOSTLOC_COUNT {
            let loc = index_to_hostloc(i);
            assert_eq!(
                hostloc_to_index(loc),
                i,
                "Round-trip failed for index {}",
                i
            );
        }
    }

    #[test]
    fn test_hostloc_info_lifecycle() {
        let mut info = HostLocInfo::new();
        assert!(info.is_empty());
        assert!(!info.is_locked());

        // Add a value
        info.add_value(InstRef(0), 64, 2);
        assert!(!info.is_empty());

        // Read lock
        info.read_lock();
        assert!(info.is_locked());

        // Release
        info.release_one();
        assert!(!info.is_locked());
        assert!(!info.is_empty()); // still has uses remaining
    }

    #[test]
    fn test_hostloc_info_last_use_cleanup() {
        let mut info = HostLocInfo::new();
        info.add_value(InstRef(0), 64, 1); // total_uses = 1
        info.add_arg_reference(None, InstRef(0), 0); // simulate GetArgumentInfo
        info.read_lock();
        info.release_one(); // current_references > 0 → counts as a use → last use → clears
        assert!(info.is_empty(), "Should be empty after last use");
    }

    #[test]
    fn test_argument_immediate() {
        let arg = Argument {
            allocated: false,
            value: Value::ImmU32(42),
        };
        assert!(arg.is_immediate());
        assert!(!arg.is_void());
        assert!(arg.fits_in_immediate_u32());
        assert!(arg.fits_in_immediate_s32());
        assert_eq!(arg.get_immediate_u32(), 42);
    }

    #[test]
    fn test_signed_32_immediate_boundaries_use_raw_bits() {
        let positive_limit = Argument {
            allocated: false,
            value: Value::ImmU32(0x7fff_ffff),
        };
        let first_positive_overflow = Argument {
            allocated: false,
            value: Value::ImmU32(0x8000_0000),
        };
        let u32_all_ones = Argument {
            allocated: false,
            value: Value::ImmU32(u32::MAX),
        };
        let u16_all_ones = Argument {
            allocated: false,
            value: Value::ImmU16(u16::MAX),
        };
        let u64_all_ones = Argument {
            allocated: false,
            value: Value::ImmU64(u64::MAX),
        };

        // Upstream tests the raw zero-extended immediate value. ImmU32::MAX
        // is therefore +4294967295, while ImmU64::MAX casts to -1.
        assert!(positive_limit.fits_in_immediate_s32());
        assert_eq!(positive_limit.get_immediate_s32(), 0x7fff_ffff);
        assert!(!first_positive_overflow.fits_in_immediate_s32());
        assert!(!u32_all_ones.fits_in_immediate_s32());
        assert!(u16_all_ones.fits_in_immediate_s32());
        assert_eq!(u16_all_ones.get_immediate_s32(), 0xffff);
        assert!(u64_all_ones.fits_in_immediate_s32());
        assert_eq!(u64_all_ones.get_immediate_s32(), u64::MAX);
    }

    #[test]
    fn test_argument_void() {
        let arg = Argument::new();
        assert!(arg.is_void());
    }

    #[test]
    fn test_select_register_prefers_empty() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let mut ra = RegAlloc::new_default(&mut asm, vec![]);

        // Add a value to the first GPR in order
        let first_gpr = ANY_GPR[0];
        ra.loc_info_mut(first_gpr).add_value(InstRef(0), 64, 1);

        // Selection should skip the occupied one
        let selected = ra.select_a_register(ANY_GPR);
        assert_ne!(selected, first_gpr, "Should prefer empty register");
        assert!(ra.loc_info(selected).is_empty());
    }

    #[test]
    fn test_scratch_gpr() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let mut ra = RegAlloc::new_default(&mut asm, vec![]);

        let reg = ra.scratch_gpr();
        assert!(reg.get_bit() == 64 || reg.get_bit() == 32);
        // The location should be write-locked
        let loc = reg_to_hostloc(reg);
        assert!(ra.loc_info(loc).is_locked());

        ra.end_of_alloc_scope();
        assert!(!ra.loc_info(loc).is_locked());
    }

    #[test]
    fn test_define_and_use_value() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let inst_info = vec![
            (1, 64), // InstRef(0): used once, 64-bit
            (0, 64), // InstRef(1): consumer instruction
        ];
        let mut ra = RegAlloc::new_default(&mut asm, inst_info);

        // Define a value (simulating an instruction that produces InstRef(0))
        let scratch = ra.scratch_gpr();
        let loc = reg_to_hostloc(scratch);
        ra.define_value(InstRef(0), scratch);
        ra.end_of_alloc_scope();

        // The value should be findable
        assert!(ra.is_value_live(InstRef(0)));
        assert_eq!(ra.value_location(InstRef(0)), Some(loc));

        // Use the value via get_argument_info (proper flow)
        let args = [Value::Inst(InstRef(0))];
        let mut arg_info = ra.get_argument_info(InstRef(1), &args, 1);
        let used_reg = ra.use_gpr(&mut arg_info[0]);
        assert!(used_reg.get_bit() == 64);
        ra.end_of_alloc_scope();

        // After last use (total_uses=1), the value should be cleaned up
        assert!(!ra.is_value_live(InstRef(0)));
    }

    #[test]
    fn register_multi_result_pseudo_operation_preserves_inline_value() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let inst_info = vec![(1, 0), (1, 128)];
        let mut ra = RegAlloc::new_default(&mut asm, inst_info);

        let result = ra.scratch_xmm();
        let result_loc = reg_to_hostloc(result);
        ra.define_value(InstRef(1), result);
        ra.end_of_alloc_scope();

        let size_before = ra.asm.size();
        ra.register_pseudo_operation(InstRef(1), &[Value::Inst(InstRef(0))], 1);

        assert_eq!(ra.asm.size(), size_before);
        assert_eq!(ra.value_location(InstRef(1)), Some(result_loc));
    }

    #[test]
    fn test_spill_and_reload() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        // Create values with 2 uses each to keep them alive
        let mut inst_info = Vec::new();
        for _ in 0..ANY_GPR.len() + 1 {
            inst_info.push((2u32, 64usize));
        }
        let mut ra = RegAlloc::new_default(&mut asm, inst_info);

        // Fill all GPRs with values
        let num_gprs = ANY_GPR.len();
        for i in 0..num_gprs {
            let scratch = ra.scratch_gpr();
            ra.define_value(InstRef(i as u32), scratch);
            ra.end_of_alloc_scope();
        }

        // Allocating one more should trigger a spill
        let extra_scratch = ra.scratch_gpr();
        ra.define_value(InstRef(num_gprs as u32), extra_scratch);
        ra.end_of_alloc_scope();

        // All values should still be live (some spilled)
        for i in 0..=num_gprs {
            assert!(
                ra.is_value_live(InstRef(i as u32)),
                "Value {} should still be live",
                i
            );
        }

        // At least one value should be in a spill slot
        let spilled_count = (0..=num_gprs)
            .filter(|&i| {
                matches!(
                    ra.value_location(InstRef(i as u32)),
                    Some(HostLoc::Spill(_))
                )
            })
            .count();
        assert!(
            spilled_count > 0,
            "At least one value should have been spilled"
        );
    }

    #[test]
    fn test_find_free_spill() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let ra = RegAlloc::new_default(&mut asm, vec![]);
        let spill = ra.find_free_spill();
        assert!(matches!(spill, HostLoc::Spill(0)));
    }

    #[test]
    fn test_xmm_gpr_128_width_matches_upstream_release_movd_fallthrough() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        {
            let mut ra = RegAlloc::new_default(&mut asm, vec![]);
            ra.emit_move(128, HostLoc::Gpr(7), HostLoc::Xmm(1));
        }
        let xmm_to_gpr = asm.code().to_vec();

        let mut expected = CodeAssembler::new(4096).unwrap();
        expected
            .movd(Reg::gpr32(7), HostLoc::Xmm(1).to_xmm())
            .unwrap();
        assert_eq!(xmm_to_gpr, expected.code());

        let mut asm = CodeAssembler::new(4096).unwrap();
        {
            let mut ra = RegAlloc::new_default(&mut asm, vec![]);
            ra.emit_move(128, HostLoc::Xmm(1), HostLoc::Gpr(7));
        }
        let gpr_to_xmm = asm.code().to_vec();

        let mut expected = CodeAssembler::new(4096).unwrap();
        expected
            .movd(HostLoc::Xmm(1).to_xmm(), Reg::gpr32(7))
            .unwrap();
        assert_eq!(gpr_to_xmm, expected.code());
    }

    #[test]
    fn test_can_exchange() {
        assert!(can_exchange(HOST_RAX, HOST_RBX));
        assert!(!can_exchange(HOST_RAX, HostLoc::Xmm(0)));
        assert!(!can_exchange(HostLoc::Xmm(1), HostLoc::Xmm(2)));
    }

    #[test]
    fn test_load_immediate_zero() {
        let mut asm = CodeAssembler::new(4096).unwrap();
        let mut ra = RegAlloc::new_default(&mut asm, vec![]);
        let loc = ra.scratch_impl(ANY_GPR);
        ra.load_immediate(Value::ImmU64(0), loc);
        // Should have emitted xor r32, r32
        assert!(ra.asm.size() > 0);
        ra.end_of_alloc_scope();
    }
}

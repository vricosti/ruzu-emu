use crate::ir::block::Block;
use crate::ir::opcode::Opcode;
use crate::ir::value::{InstRef, Value};

/// A32 Get/Set Elimination pass.
///
/// Two-pass approach matching upstream `Dynarmic::Optimization::A32GetSetElimination`:
/// 1. FlagsPass: reverse iteration — eliminates redundant CPSR flag operations
/// 2. RegisterPass: forward iteration — eliminates redundant register Get/Set pairs
pub fn a32_get_set_elimination(block: &mut Block) {
    flags_pass(block);
    register_pass(block);
}

// ---------------------------------------------------------------------------
// FlagsPass — reverse iteration over flag operations
// ---------------------------------------------------------------------------
// Upstream: `ir/opt_passes.cpp`, `FlagsPass`.

struct FlagInfo {
    set_not_required: bool,
    has_value_request: bool,
    value_request_idx: usize,
}

impl FlagInfo {
    fn new() -> Self {
        Self {
            set_not_required: false,
            has_value_request: false,
            value_request_idx: 0,
        }
    }
    fn set_not_required() -> Self {
        Self {
            set_not_required: true,
            has_value_request: false,
            value_request_idx: 0,
        }
    }
}

struct ValuelessFlagInfo {
    set_not_required: bool,
}

impl ValuelessFlagInfo {
    fn new() -> Self {
        Self {
            set_not_required: false,
        }
    }
    fn set_not_required() -> Self {
        Self {
            set_not_required: true,
        }
    }
}

fn flags_do_set(block: &mut Block, info: &mut FlagInfo, value: Value, inst_idx: usize) {
    if info.has_value_request {
        let req = InstRef(info.value_request_idx as u32);
        block.replace_uses_with(req, value);
    }
    info.has_value_request = false;

    if info.set_not_required {
        block.invalidate(InstRef(inst_idx as u32));
    }
    info.set_not_required = true;
}

fn flags_do_set_valueless(block: &mut Block, info: &mut ValuelessFlagInfo, inst_idx: usize) {
    if info.set_not_required {
        block.invalidate(InstRef(inst_idx as u32));
    }
    info.set_not_required = true;
}

fn flags_do_get(block: &mut Block, info: &mut FlagInfo, inst_idx: usize) {
    if info.has_value_request {
        let old_req = InstRef(info.value_request_idx as u32);
        block.replace_uses_with(old_req, Value::Inst(InstRef(inst_idx as u32)));
    }
    info.has_value_request = true;
    info.value_request_idx = inst_idx;
}

fn flags_pass(block: &mut Block) {
    let mut nzcvq = ValuelessFlagInfo::new();
    let mut nzcv = ValuelessFlagInfo::new();
    let mut nz = ValuelessFlagInfo::new();
    let mut c_flag = FlagInfo::new();
    let mut ge = FlagInfo::new();

    let mut i = block.instructions.len();
    while i > 0 {
        i -= 1;

        if block.instructions[i].is_tombstone() || block.instructions[i].opcode == Opcode::Identity
        {
            continue;
        }

        let opcode = block.instructions[i].opcode;

        match opcode {
            Opcode::A32GetCFlag => {
                flags_do_get(block, &mut c_flag, i);
            }

            Opcode::A32SetCpsrNZCV => {
                let mut set_idx = i;
                if c_flag.has_value_request {
                    let mut req = InstRef(c_flag.value_request_idx as u32);
                    let nzcv_arg = block.instructions[i].args[0];
                    let c = block.insert(i, Opcode::GetCFlagFromNZCV, &[nzcv_arg]);
                    if req.index() >= i {
                        req.0 += 1;
                    }
                    block.replace_uses_with(req, Value::Inst(c));
                    set_idx += 1;
                }

                flags_do_set_valueless(block, &mut nzcv, set_idx);
                nz = ValuelessFlagInfo::set_not_required();
                c_flag = FlagInfo::set_not_required();
            }

            Opcode::A32SetCpsrNZCVRaw => {
                if c_flag.has_value_request {
                    nzcv.set_not_required = false;
                }

                flags_do_set_valueless(block, &mut nzcv, i);
                nzcvq = ValuelessFlagInfo::new();
                nz = ValuelessFlagInfo::set_not_required();
                c_flag = FlagInfo::set_not_required();
            }

            Opcode::A32SetCpsrNZCVQ => {
                if c_flag.has_value_request {
                    nzcvq.set_not_required = false;
                }

                flags_do_set_valueless(block, &mut nzcvq, i);
                nzcv = ValuelessFlagInfo::set_not_required();
                nz = ValuelessFlagInfo::set_not_required();
                c_flag = FlagInfo::set_not_required();
            }

            Opcode::A32SetCpsrNZ => {
                flags_do_set_valueless(block, &mut nz, i);
                nzcvq = ValuelessFlagInfo::new();
                nzcv = ValuelessFlagInfo::new();
            }

            Opcode::A32SetCpsrNZC => {
                // Forward C flag value to pending request
                if c_flag.has_value_request {
                    let c_arg = block.instructions[i].args[1];
                    let req = InstRef(c_flag.value_request_idx as u32);
                    block.replace_uses_with(req, c_arg);
                    c_flag.has_value_request = false;
                }

                // If C arg is GetCFlag (setting C to itself) → downgrade to SetCpsrNZ
                let c_arg = block.instructions[i].args[1];
                if let Some(c_opcode) = get_recursive_opcode(block, c_arg) {
                    if c_opcode == Opcode::A32GetCFlag {
                        // Match upstream intent: drop the explicit C input and keep only NZ.
                        // We rewrite in place to preserve ordering within the block.
                        block.instructions[i].opcode = Opcode::A32SetCpsrNZ;
                        block.set_arg(InstRef(i as u32), 1, Value::Void);

                        nzcvq = ValuelessFlagInfo::new();
                        nzcv = ValuelessFlagInfo::new();
                        nz = ValuelessFlagInfo::set_not_required();
                        continue;
                    }
                }

                if nz.set_not_required && c_flag.set_not_required {
                    block.invalidate(InstRef(i as u32));
                } else if nz.set_not_required {
                    block.set_arg(InstRef(i as u32), 0, Value::empty_nzcv_immediate_marker());
                }
                nz.set_not_required = true;
                c_flag = FlagInfo::set_not_required();
                nzcv = ValuelessFlagInfo::new();
                nzcvq = ValuelessFlagInfo::new();
            }

            Opcode::A32SetGEFlags => {
                let value = block.instructions[i].args[0];
                flags_do_set(block, &mut ge, value, i);
            }

            Opcode::A32GetGEFlags => {
                flags_do_get(block, &mut ge, i);
            }

            Opcode::A32SetGEFlagsCompressed => {
                ge = FlagInfo::set_not_required();
            }

            Opcode::A32OrQFlag => {
                // No-op — matches upstream.
            }

            _ => {
                if block.instructions[i].opcode.reads_cpsr()
                    || block.instructions[i].opcode.writes_cpsr()
                {
                    nzcvq = ValuelessFlagInfo::new();
                    nzcv = ValuelessFlagInfo::new();
                    nz = ValuelessFlagInfo::new();
                    c_flag = FlagInfo::new();
                    ge = FlagInfo::new();
                }
            }
        }
    }
}

/// Chase through Identity instructions to find the underlying opcode.
/// Matches upstream `Value::GetInstRecursive()`.
fn get_recursive_opcode(block: &Block, value: Value) -> Option<Opcode> {
    match value {
        Value::Inst(r) => {
            let inst = &block.instructions[r.index()];
            if inst.opcode == Opcode::Identity {
                get_recursive_opcode(block, inst.args[0])
            } else {
                Some(inst.opcode)
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// RegisterPass — forward iteration over register Get/Set pairs
// ---------------------------------------------------------------------------
// Upstream: `ir/opt_passes.cpp`, `RegisterPass`.

struct RegInfo {
    register_value: Option<Value>,
    last_set_index: Option<usize>,
}

impl RegInfo {
    fn new() -> Self {
        Self {
            register_value: None,
            last_set_index: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExtValueType {
    Empty,
    Single,
    Double,
    VectorDouble,
    VectorQuad,
}

struct ExtRegInfo {
    value_type: ExtValueType,
    register_value: Option<Value>,
    last_set_index: Option<usize>,
}

impl ExtRegInfo {
    fn new() -> Self {
        Self {
            value_type: ExtValueType::Empty,
            register_value: None,
            last_set_index: None,
        }
    }
}

fn register_pass(block: &mut Block) {
    let mut reg_info: [RegInfo; 15] = std::array::from_fn(|_| RegInfo::new());
    let mut ext_reg_info: [ExtRegInfo; 64] = std::array::from_fn(|_| ExtRegInfo::new());

    let mut i = 0;
    while i < block.instructions.len() {
        if block.instructions[i].is_tombstone() || block.instructions[i].opcode == Opcode::Identity
        {
            i += 1;
            continue;
        }

        let opcode = block.instructions[i].opcode;
        let inst_ref = InstRef(i as u32);

        match opcode {
            // ---- Core registers ----
            Opcode::A32GetRegister => {
                let reg_idx = a32_reg_index(block, i);
                if reg_idx < 15 {
                    let info = &reg_info[reg_idx];
                    if info.register_value.is_some() {
                        let val = info.register_value.unwrap();
                        block.replace_uses_with(inst_ref, val);
                    } else {
                        reg_info[reg_idx].register_value = Some(Value::Inst(inst_ref));
                    }
                }
            }

            Opcode::A32SetRegister => {
                let reg_idx = a32_reg_index(block, i);
                if reg_idx < 15 {
                    let value = block.instructions[i].args[1];
                    if let Some(prev_set) = reg_info[reg_idx].last_set_index {
                        block.invalidate(InstRef(prev_set as u32));
                    }
                    reg_info[reg_idx] = RegInfo {
                        register_value: Some(value),
                        last_set_index: Some(i),
                    };
                }
            }

            // ---- Extended registers (Single) ----
            Opcode::A32GetExtendedRegister32 => {
                let backing = a32_ext_backing(block, i);
                if backing < 64 {
                    ext_do_get(
                        block,
                        &mut ext_reg_info,
                        ExtValueType::Single,
                        &[backing],
                        inst_ref,
                    );
                }
            }
            Opcode::A32SetExtendedRegister32 => {
                let backing = a32_ext_backing(block, i);
                if backing < 64 {
                    let value = block.instructions[i].args[1];
                    ext_do_set(
                        block,
                        &mut ext_reg_info,
                        ExtValueType::Single,
                        &[backing],
                        value,
                        i,
                    );
                }
            }

            // ---- Extended registers (Double) ----
            Opcode::A32GetExtendedRegister64 => {
                let ext_reg = block.instructions[i].args[0].get_a32_ext_reg();
                let reg_num = ext_reg.index();
                let slots = [reg_num * 2, reg_num * 2 + 1];
                if *slots.last().unwrap() < 64 {
                    ext_do_get(
                        block,
                        &mut ext_reg_info,
                        ExtValueType::Double,
                        &slots,
                        inst_ref,
                    );
                }
            }
            Opcode::A32SetExtendedRegister64 => {
                let ext_reg = block.instructions[i].args[0].get_a32_ext_reg();
                let reg_num = ext_reg.index();
                let slots = [reg_num * 2, reg_num * 2 + 1];
                if *slots.last().unwrap() < 64 {
                    let value = block.instructions[i].args[1];
                    ext_do_set(
                        block,
                        &mut ext_reg_info,
                        ExtValueType::Double,
                        &slots,
                        value,
                        i,
                    );
                }
            }

            // ---- Vector registers ----
            Opcode::A32GetVector => {
                let ext_reg = block.instructions[i].args[0].get_a32_ext_reg();
                if ext_reg.is_double() {
                    let reg_num = ext_reg.index();
                    let slots = [reg_num * 2, reg_num * 2 + 1];
                    if *slots.last().unwrap() < 64 {
                        ext_do_get(
                            block,
                            &mut ext_reg_info,
                            ExtValueType::VectorDouble,
                            &slots,
                            inst_ref,
                        );
                    }
                } else {
                    let reg_num = ext_reg.index();
                    let slots = [
                        reg_num * 4,
                        reg_num * 4 + 1,
                        reg_num * 4 + 2,
                        reg_num * 4 + 3,
                    ];
                    if *slots.last().unwrap() < 64 {
                        ext_do_get(
                            block,
                            &mut ext_reg_info,
                            ExtValueType::VectorQuad,
                            &slots,
                            inst_ref,
                        );
                    }
                }
            }
            Opcode::A32SetVector => {
                let ext_reg = block.instructions[i].args[0].get_a32_ext_reg();
                let value = block.instructions[i].args[1];
                if ext_reg.is_double() {
                    let reg_num = ext_reg.index();
                    let slots = [reg_num * 2, reg_num * 2 + 1];
                    if *slots.last().unwrap() < 64 {
                        let stored_value = block.insert(i + 1, Opcode::VectorZeroUpper, &[value]);
                        ext_do_set(
                            block,
                            &mut ext_reg_info,
                            ExtValueType::VectorDouble,
                            &slots,
                            Value::Inst(stored_value),
                            i,
                        );
                    }
                } else {
                    let reg_num = ext_reg.index();
                    let slots = [
                        reg_num * 4,
                        reg_num * 4 + 1,
                        reg_num * 4 + 2,
                        reg_num * 4 + 3,
                    ];
                    if *slots.last().unwrap() < 64 {
                        ext_do_set(
                            block,
                            &mut ext_reg_info,
                            ExtValueType::VectorQuad,
                            &slots,
                            value,
                            i,
                        );
                    }
                }
            }

            _ => {
                if block.instructions[i].opcode.reads_from_core_register()
                    || block.instructions[i].opcode.writes_to_core_register()
                {
                    reg_info = std::array::from_fn(|_| RegInfo::new());
                    ext_reg_info = std::array::from_fn(|_| ExtRegInfo::new());
                }
            }
        }
        i += 1;
    }
}

// ---- RegisterPass ext-reg helpers ----

/// Extended register do_get: check if all slots have matching type, replace or record.
/// Matches upstream `do_ext_get`.
fn ext_do_get(
    block: &mut Block,
    info: &mut [ExtRegInfo; 64],
    expected_type: ExtValueType,
    slots: &[usize],
    get_inst: InstRef,
) {
    let all_match = slots.iter().all(|&s| info[s].value_type == expected_type);
    if !all_match {
        // Type mismatch — record this Get as the new value for all slots
        for &s in slots {
            info[s] = ExtRegInfo {
                value_type: expected_type,
                register_value: Some(Value::Inst(get_inst)),
                last_set_index: None,
            };
        }
        return;
    }
    // All slots match — replace Get with the known value from the first slot
    if let Some(val) = info[slots[0]].register_value {
        block.replace_uses_with(get_inst, val);
    }
}

/// Extended register do_set: invalidate previous dead set, update all slots.
/// Matches upstream `do_ext_set`.
fn ext_do_set(
    block: &mut Block,
    info: &mut [ExtRegInfo; 64],
    expected_type: ExtValueType,
    slots: &[usize],
    value: Value,
    set_idx: usize,
) {
    let all_match = slots.iter().all(|&s| info[s].value_type == expected_type);
    if all_match {
        // Same type — invalidate previous dead set if any
        if let Some(prev) = info[slots[0]].last_set_index {
            block.invalidate(InstRef(prev as u32));
        }
    }
    for &s in slots {
        info[s] = ExtRegInfo {
            value_type: expected_type,
            register_value: Some(value),
            last_set_index: Some(set_idx),
        };
    }
}

// ---- Index helpers ----

fn a32_reg_index(block: &Block, inst_idx: usize) -> usize {
    block.instructions[inst_idx].args[0].get_a32_reg().number()
}

fn a32_ext_backing(block: &Block, inst_idx: usize) -> usize {
    block.instructions[inst_idx].args[0]
        .get_a32_ext_reg()
        .backing_index()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::translate::translate;
    use crate::ir::block::Block;
    use crate::ir::location::A32LocationDescriptor;
    use crate::ir::location::LocationDescriptor;
    use crate::ir::opt::identity_removal::identity_removal;
    use crate::ir::opt::verification::verification_pass;

    /// Helper to create a test block and run GSE + identity removal.
    fn run_gse(block: &mut Block) {
        a32_get_set_elimination(block);
        identity_removal(block);
    }

    #[test]
    fn test_register_get_set_elimination() {
        use crate::frontend::a32::types::Reg;

        let mut block = Block::new(LocationDescriptor(0x1000));

        // SetRegister(R0, imm 42)
        block.append(
            Opcode::A32SetRegister,
            &[Value::ImmA32Reg(Reg::R0), Value::ImmU32(42)],
        );

        // GetRegister(R0) → should be eliminated, replaced with imm 42
        let get = block.append(Opcode::A32GetRegister, &[Value::ImmA32Reg(Reg::R0)]);

        // Use the Get result in an Add
        let add = block.append(
            Opcode::Add32,
            &[Value::Inst(get), Value::ImmU32(1), Value::ImmU1(false)],
        );

        // SetRegister(R1, add_result)
        block.append(
            Opcode::A32SetRegister,
            &[Value::ImmA32Reg(Reg::R1), Value::Inst(add)],
        );

        run_gse(&mut block);

        // After GSE + identity removal: the Add should reference ImmU32(42) directly
        let add_inst = block.get(add);
        assert_eq!(
            add_inst.args[0],
            Value::ImmU32(42),
            "GetRegister should be eliminated; Add should use immediate 42"
        );
    }

    #[test]
    fn test_dead_set_elimination() {
        use crate::frontend::a32::types::Reg;

        let mut block = Block::new(LocationDescriptor(0x1000));

        // SetRegister(R0, 10) — dead store (overwritten before any Get)
        let dead_set = block.append(
            Opcode::A32SetRegister,
            &[Value::ImmA32Reg(Reg::R0), Value::ImmU32(10)],
        );

        // SetRegister(R0, 20) — overwrites above
        block.append(
            Opcode::A32SetRegister,
            &[Value::ImmA32Reg(Reg::R0), Value::ImmU32(20)],
        );

        run_gse(&mut block);

        // The first SetRegister should be tombstoned
        assert!(
            block.get(dead_set).is_tombstone(),
            "Dead store (SetRegister R0=10) should be eliminated"
        );
    }

    #[test]
    fn test_chained_get_set() {
        use crate::frontend::a32::types::Reg;

        let mut block = Block::new(LocationDescriptor(0x1000));

        // GetRegister(R2) — first read
        let get_r2 = block.append(Opcode::A32GetRegister, &[Value::ImmA32Reg(Reg::R2)]);

        // Compute something
        let result = block.append(
            Opcode::Add32,
            &[Value::Inst(get_r2), Value::ImmU32(1), Value::ImmU1(false)],
        );

        // SetRegister(R2, result)
        block.append(
            Opcode::A32SetRegister,
            &[Value::ImmA32Reg(Reg::R2), Value::Inst(result)],
        );

        // GetRegister(R2) — should get result, not re-read
        let get_r2_again = block.append(Opcode::A32GetRegister, &[Value::ImmA32Reg(Reg::R2)]);

        // Use it
        block.append(
            Opcode::A32SetRegister,
            &[Value::ImmA32Reg(Reg::R3), Value::Inst(get_r2_again)],
        );

        run_gse(&mut block);

        // After GSE: second GetRegister(R2) should be replaced with the result of Add32
        let set_r3 = &block.instructions[4];
        assert_eq!(
            set_r3.args[1],
            Value::Inst(result),
            "Second GetRegister(R2) should be replaced with Add32 result"
        );
    }

    #[test]
    fn test_register_pass_ignores_pc_without_stalling() {
        use crate::frontend::a32::types::Reg;

        let mut block = Block::new(LocationDescriptor(0x1000));

        block.append(
            Opcode::A32SetRegister,
            &[Value::ImmA32Reg(Reg::PC), Value::ImmU32(0x2000)],
        );
        let get_r0 = block.append(Opcode::A32GetRegister, &[Value::ImmA32Reg(Reg::R0)]);
        block.append(
            Opcode::A32SetRegister,
            &[Value::ImmA32Reg(Reg::R1), Value::Inst(get_r0)],
        );

        run_gse(&mut block);
        verification_pass(&block);

        assert!(
            block
                .iter_live()
                .any(|(_, inst)| inst.opcode == Opcode::A32SetRegister
                    && inst.args[0] == Value::ImmA32Reg(Reg::PC)),
            "RegisterPass must skip PC like upstream instead of tracking or stalling on it"
        );
    }

    #[test]
    fn test_set_cpsr_nzc_get_cflag_rewrite_keeps_use_counts_consistent() {
        let mut block = Block::new(LocationDescriptor(0x1000));

        let add = block.append(
            Opcode::Add32,
            &[Value::ImmU32(1), Value::ImmU32(2), Value::ImmU1(false)],
        );
        let nz = block.append(Opcode::GetNZFromOp, &[Value::Inst(add)]);
        let get_c_1 = block.append(Opcode::A32GetCFlag, &[]);
        block.append(
            Opcode::A32SetCpsrNZC,
            &[Value::Inst(nz), Value::Inst(get_c_1)],
        );
        let get_c_2 = block.append(Opcode::A32GetCFlag, &[]);
        block.append(Opcode::A32SetCheckBit, &[Value::Inst(get_c_2)]);

        run_gse(&mut block);
        verification_pass(&block);

        let live_get_c_flags = block
            .iter_live()
            .filter(|(_, inst)| inst.opcode == Opcode::A32GetCFlag)
            .count();
        assert_eq!(live_get_c_flags, 1, "one carried C read should remain live");

        for (_, inst) in block.iter_live() {
            if inst.opcode == Opcode::A32GetCFlag {
                assert_eq!(inst.use_count, 1, "live A32GetCFlag must have one real use");
            }
        }
    }

    #[test]
    fn test_set_cpsr_nzcv_with_pending_get_cflag_does_not_create_forward_refs() {
        let mut block = Block::new(LocationDescriptor(0x1000));

        let add = block.append(
            Opcode::Add32,
            &[Value::ImmU32(1), Value::ImmU32(2), Value::ImmU1(false)],
        );
        let nzcv = block.append(Opcode::GetNZCVFromOp, &[Value::Inst(add)]);
        block.append(Opcode::A32SetCpsrNZCV, &[Value::Inst(nzcv)]);
        let get_c = block.append(Opcode::A32GetCFlag, &[]);
        block.append(Opcode::A32SetCheckBit, &[Value::Inst(get_c)]);

        run_gse(&mut block);
        verification_pass(&block);

        for (i, inst) in block.iter_live() {
            for arg in inst.arg_values() {
                if let Value::Inst(r) = *arg {
                    assert!(
                        r.index() < i.index(),
                        "forward reference created at inst #{} -> #{}",
                        i.index(),
                        r.index()
                    );
                }
            }
        }

        let mut inserted_get_idx = None;
        let mut set_nzcv_idx = None;
        for (i, inst) in block.iter_live() {
            if inst.opcode == Opcode::GetCFlagFromNZCV {
                inserted_get_idx = Some(i.index());
            }
            if inst.opcode == Opcode::A32SetCpsrNZCV {
                set_nzcv_idx = Some(i.index());
            }
        }
        assert!(
            inserted_get_idx.is_some(),
            "pending A32GetCFlag should be replaced by GetCFlagFromNZCV"
        );
        assert!(
            inserted_get_idx.unwrap() < set_nzcv_idx.unwrap(),
            "GetCFlagFromNZCV must be inserted before A32SetCpsrNZCV like upstream"
        );
    }

    #[test]
    fn test_update_upper_location_descriptor_blocks_cflag_forwarding() {
        let mut block = Block::new(LocationDescriptor(0x1000));

        let add = block.append(
            Opcode::Add32,
            &[Value::ImmU32(1), Value::ImmU32(2), Value::ImmU1(false)],
        );
        let nzcv = block.append(Opcode::GetNZCVFromOp, &[Value::Inst(add)]);
        block.append(Opcode::A32SetCpsrNZCV, &[Value::Inst(nzcv)]);
        block.append(Opcode::A32UpdateUpperLocationDescriptor, &[]);
        let get_c = block.append(Opcode::A32GetCFlag, &[]);
        block.append(Opcode::A32SetCheckBit, &[Value::Inst(get_c)]);

        run_gse(&mut block);
        verification_pass(&block);

        assert!(
            block
                .iter_live()
                .any(|(_, inst)| inst.opcode == Opcode::A32GetCFlag),
            "A32UpdateUpperLocationDescriptor reads CPSR upstream and must stop C flag forwarding"
        );
        assert!(
            block
                .iter_live()
                .all(|(_, inst)| inst.opcode != Opcode::GetCFlagFromNZCV),
            "C flag forwarding must not cross A32UpdateUpperLocationDescriptor"
        );
    }

    #[test]
    fn test_set_cpsr_nzc_rewrites_dead_nz_input_to_empty_marker() {
        let mut block = Block::new(LocationDescriptor(0x1000));

        let add_1 = block.append(
            Opcode::Add32,
            &[Value::ImmU32(1), Value::ImmU32(2), Value::ImmU1(false)],
        );
        let nz_1 = block.append(Opcode::GetNZFromOp, &[Value::Inst(add_1)]);
        block.append(
            Opcode::A32SetCpsrNZC,
            &[Value::Inst(nz_1), Value::ImmU1(true)],
        );

        let add_2 = block.append(
            Opcode::Add32,
            &[Value::ImmU32(3), Value::ImmU32(4), Value::ImmU1(false)],
        );
        let nz_2 = block.append(Opcode::GetNZFromOp, &[Value::Inst(add_2)]);
        block.append(Opcode::A32SetCpsrNZ, &[Value::Inst(nz_2)]);

        let get_c = block.append(Opcode::A32GetCFlag, &[]);
        block.append(Opcode::A32SetCheckBit, &[Value::Inst(get_c)]);

        run_gse(&mut block);
        verification_pass(&block);

        let set_nzc = block
            .iter_live()
            .find_map(|(_, inst)| (inst.opcode == Opcode::A32SetCpsrNZC).then_some(inst))
            .expect("A32SetCpsrNZC should remain live when only C is needed");

        assert_eq!(
            set_nzc.args[0],
            Value::empty_nzcv_immediate_marker(),
            "dead NZ input should be rewritten to EmptyNZCVImmediateMarker"
        );
    }

    #[test]
    fn test_double_vector_set_forwards_inserted_zero_upper_value() {
        use crate::frontend::a32::types::ExtReg;

        let mut block = Block::new(LocationDescriptor(0x1000));

        let src = block.append(Opcode::ZeroVector, &[]);
        block.append(
            Opcode::A32SetVector,
            &[Value::ImmA32ExtReg(ExtReg::D0), Value::Inst(src)],
        );
        let get = block.append(Opcode::A32GetVector, &[Value::ImmA32ExtReg(ExtReg::D0)]);
        block.append(Opcode::VectorBroadcastLower8, &[Value::Inst(get)]);

        run_gse(&mut block);
        verification_pass(&block);

        let broadcast_inst = block
            .iter_live()
            .find_map(|(_, inst)| (inst.opcode == Opcode::VectorBroadcastLower8).then_some(inst))
            .expect("broadcast should remain live");
        let broadcast_arg = broadcast_inst.args[0].inst_ref();
        assert_eq!(
            block.get(broadcast_arg).opcode,
            Opcode::VectorZeroUpper,
            "double-vector Get users should be rewritten to the inserted VectorZeroUpper value"
        );
    }

    #[test]
    fn test_cmp_block_keeps_final_nzcv_write_for_following_conditional_branch() {
        let loc = A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), false);
        let read_code = |addr: u32| match addr {
            0x1000 => Some(0xE2877001),
            0x1004 => Some(0xEEB48AC0),
            0x1008 => Some(0xEEF1FA10),
            0x100C => Some(0xE1540007),
            0x1010 => Some(0x1AFFFFFA),
            _ => None,
        };

        let mut block = translate(
            loc,
            &read_code,
            crate::frontend::a32::translate::TranslationOptions::default(),
        );
        run_gse(&mut block);
        verification_pass(&block);

        assert_eq!(block.end_location.0, 0x1010);
        let set_nzcv = block
            .iter_live()
            .find_map(|(_, inst)| (inst.opcode == Opcode::A32SetCpsrNZCV).then_some(inst))
            .expect(
                "final CMP flags write must remain live for the following conditional branch block",
            );
        let Value::Inst(nzcv_ref) = set_nzcv.args[0] else {
            panic!("A32SetCpsrNZCV must keep a GetNZCVFromOp pseudo-op input");
        };
        assert_eq!(block.get(nzcv_ref).opcode, Opcode::GetNZCVFromOp);
    }
}

use crate::frontend::a64::types::{Exception, Reg, Vec};
use crate::interface::a64::config::{DataCacheOperation, InstructionCacheOperation};
use crate::ir::acc_type::AccType;
use crate::ir::block::Block;
use crate::ir::emitter::IREmitter;
use crate::ir::location::A64LocationDescriptor;
use crate::ir::opcode::Opcode;
use crate::ir::terminal::Terminal;
use crate::ir::types::Type;
use crate::ir::value::Value;

/// A64-specific IR emitter. Extends IREmitter with A64 register/memory/system operations.
pub struct A64IREmitter<'a> {
    pub base: IREmitter<'a>,
    pub current_location: Option<A64LocationDescriptor>,
}

impl<'a> A64IREmitter<'a> {
    pub fn new(block: &'a mut Block) -> Self {
        Self {
            base: IREmitter::new(block),
            current_location: None,
        }
    }

    pub fn with_location(block: &'a mut Block, location: A64LocationDescriptor) -> Self {
        Self {
            base: IREmitter::new(block),
            current_location: Some(location),
        }
    }

    pub fn pc(&self) -> u64 {
        self.current_location
            .expect("current_location not set")
            .pc()
    }

    pub fn align_pc(&self, alignment: u64) -> u64 {
        self.pc() & !(alignment - 1)
    }

    /// Access the underlying base emitter for generic operations.
    pub fn ir(&mut self) -> &mut IREmitter<'a> {
        &mut self.base
    }

    /// Set block terminal.
    pub fn set_term(&mut self, terminal: Terminal) {
        self.base.set_term(terminal);
    }

    // --- Internal emit helpers ---

    fn emit(&mut self, opcode: Opcode, args: &[Value]) -> Value {
        let r = self.base.block.append(opcode, args);
        Value::Inst(r)
    }

    fn emit_void(&mut self, opcode: Opcode, args: &[Value]) {
        self.base.block.append(opcode, args);
    }

    fn imm_current_location_descriptor(&mut self) -> Value {
        let loc = self.current_location.expect("current_location not set");
        Value::ImmU64(loc.unique_hash())
    }

    fn value_type(&self, value: Value) -> Type {
        match value {
            Value::Inst(inst_ref) => self.base.block.inst_real_return_type(inst_ref),
            value => value.get_type(),
        }
    }

    fn coerce_to_u8(&mut self, value: Value) -> Value {
        match self.value_type(value) {
            Type::U8 => value,
            Type::U32 => match value {
                Value::ImmU32(imm) => Value::ImmU8(imm as u8),
                value => self.base.least_significant_byte(value),
            },
            ty => panic!("A64 WriteMemory8 value must be U8 or U32, got {:?}", ty),
        }
    }

    fn coerce_to_u16(&mut self, value: Value) -> Value {
        match self.value_type(value) {
            Type::U16 => value,
            Type::U32 => match value {
                Value::ImmU32(imm) => Value::ImmU16(imm as u16),
                value => self.base.least_significant_half(value),
            },
            ty => panic!("A64 WriteMemory16 value must be U16 or U32, got {:?}", ty),
        }
    }

    // --- A64 register getters ---

    pub fn get_w(&mut self, reg: Reg) -> Value {
        self.emit(Opcode::A64GetW, &[Value::ImmA64Reg(reg)])
    }

    pub fn get_x(&mut self, reg: Reg) -> Value {
        self.emit(Opcode::A64GetX, &[Value::ImmA64Reg(reg)])
    }

    pub fn get_s(&mut self, vec: Vec) -> Value {
        self.emit(Opcode::A64GetS, &[Value::ImmA64Vec(vec)])
    }

    pub fn get_d(&mut self, vec: Vec) -> Value {
        self.emit(Opcode::A64GetD, &[Value::ImmA64Vec(vec)])
    }

    pub fn get_q(&mut self, vec: Vec) -> Value {
        self.emit(Opcode::A64GetQ, &[Value::ImmA64Vec(vec)])
    }

    pub fn get_sp(&mut self) -> Value {
        self.emit(Opcode::A64GetSP, &[])
    }

    pub fn get_fpcr(&mut self) -> Value {
        self.emit(Opcode::A64GetFPCR, &[])
    }

    pub fn get_fpsr(&mut self) -> Value {
        self.emit(Opcode::A64GetFPSR, &[])
    }

    // --- A64 register setters ---

    pub fn set_w(&mut self, reg: Reg, value: Value) {
        self.emit_void(Opcode::A64SetW, &[Value::ImmA64Reg(reg), value]);
    }

    pub fn set_x(&mut self, reg: Reg, value: Value) {
        self.emit_void(Opcode::A64SetX, &[Value::ImmA64Reg(reg), value]);
    }

    pub fn set_s(&mut self, vec: Vec, value: Value) {
        self.emit_void(Opcode::A64SetS, &[Value::ImmA64Vec(vec), value]);
    }

    pub fn set_d(&mut self, vec: Vec, value: Value) {
        self.emit_void(Opcode::A64SetD, &[Value::ImmA64Vec(vec), value]);
    }

    pub fn set_q(&mut self, vec: Vec, value: Value) {
        self.emit_void(Opcode::A64SetQ, &[Value::ImmA64Vec(vec), value]);
    }

    pub fn set_sp(&mut self, value: Value) {
        self.emit_void(Opcode::A64SetSP, &[value]);
    }

    pub fn set_pc(&mut self, value: Value) {
        self.emit_void(Opcode::A64SetPC, &[value]);
    }

    pub fn set_fpcr(&mut self, value: Value) {
        self.emit_void(Opcode::A64SetFPCR, &[value]);
    }

    pub fn set_fpsr(&mut self, value: Value) {
        self.emit_void(Opcode::A64SetFPSR, &[value]);
    }

    // --- Flags ---

    pub fn set_check_bit(&mut self, value: Value) {
        self.emit_void(Opcode::A64SetCheckBit, &[value]);
    }

    pub fn get_c_flag(&mut self) -> Value {
        self.emit(Opcode::A64GetCFlag, &[])
    }

    pub fn get_nzcv_raw(&mut self) -> Value {
        self.emit(Opcode::A64GetNZCVRaw, &[])
    }

    pub fn set_nzcv_raw(&mut self, value: Value) {
        self.emit_void(Opcode::A64SetNZCVRaw, &[value]);
    }

    pub fn set_nzcv(&mut self, nzcv: Value) {
        self.emit_void(Opcode::A64SetNZCV, &[nzcv]);
    }

    // --- System ---

    pub fn call_supervisor(&mut self, imm: u32) {
        self.emit_void(Opcode::A64CallSupervisor, &[Value::ImmU32(imm)]);
    }

    pub fn exception_raised(&mut self, exception: Exception) {
        let pc = Value::ImmU64(self.pc());
        let exc = Value::ImmU64(exception as u64);
        self.emit_void(Opcode::A64ExceptionRaised, &[pc, exc]);
    }

    pub fn data_cache_operation_raised(&mut self, op: DataCacheOperation, value: Value) {
        let location = self.imm_current_location_descriptor();
        self.emit_void(
            Opcode::A64DataCacheOperationRaised,
            &[location, Value::ImmU64(op as u64), value],
        );
    }

    pub fn instruction_cache_operation_raised(
        &mut self,
        op: InstructionCacheOperation,
        value: Value,
    ) {
        self.emit_void(
            Opcode::A64InstructionCacheOperationRaised,
            &[Value::ImmU64(op as u64), value],
        );
    }

    pub fn data_synchronization_barrier(&mut self) {
        self.emit_void(Opcode::A64DataSynchronizationBarrier, &[]);
    }

    pub fn data_memory_barrier(&mut self) {
        self.emit_void(Opcode::A64DataMemoryBarrier, &[]);
    }

    pub fn instruction_synchronization_barrier(&mut self) {
        self.emit_void(Opcode::A64InstructionSynchronizationBarrier, &[]);
    }

    pub fn get_cntfrq(&mut self) -> Value {
        self.emit(Opcode::A64GetCNTFRQ, &[])
    }

    pub fn get_cntpct(&mut self) -> Value {
        self.emit(Opcode::A64GetCNTPCT, &[])
    }

    pub fn get_ctr(&mut self) -> Value {
        self.emit(Opcode::A64GetCTR, &[])
    }

    pub fn get_dczid(&mut self) -> Value {
        self.emit(Opcode::A64GetDCZID, &[])
    }

    pub fn get_tpidr(&mut self) -> Value {
        self.emit(Opcode::A64GetTPIDR, &[])
    }

    pub fn set_tpidr(&mut self, value: Value) {
        self.emit_void(Opcode::A64SetTPIDR, &[value]);
    }

    pub fn get_tpidrro(&mut self) -> Value {
        self.emit(Opcode::A64GetTPIDRRO, &[])
    }

    // --- Memory ---

    pub fn clear_exclusive(&mut self) {
        self.emit_void(Opcode::A64ClearExclusive, &[]);
    }

    pub fn read_memory_8(&mut self, vaddr: Value, acc_type: AccType) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ReadMemory8,
            &[upper, vaddr, Value::ImmAccType(acc_type)],
        )
    }

    pub fn read_memory_16(&mut self, vaddr: Value, acc_type: AccType) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ReadMemory16,
            &[upper, vaddr, Value::ImmAccType(acc_type)],
        )
    }

    pub fn read_memory_32(&mut self, vaddr: Value, acc_type: AccType) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ReadMemory32,
            &[upper, vaddr, Value::ImmAccType(acc_type)],
        )
    }

    pub fn read_memory_64(&mut self, vaddr: Value, acc_type: AccType) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ReadMemory64,
            &[upper, vaddr, Value::ImmAccType(acc_type)],
        )
    }

    pub fn read_memory_128(&mut self, vaddr: Value, acc_type: AccType) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ReadMemory128,
            &[upper, vaddr, Value::ImmAccType(acc_type)],
        )
    }

    pub fn exclusive_read_memory_8(&mut self, vaddr: Value, acc_type: AccType) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ExclusiveReadMemory8,
            &[upper, vaddr, Value::ImmAccType(acc_type)],
        )
    }

    pub fn exclusive_read_memory_16(&mut self, vaddr: Value, acc_type: AccType) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ExclusiveReadMemory16,
            &[upper, vaddr, Value::ImmAccType(acc_type)],
        )
    }

    pub fn exclusive_read_memory_32(&mut self, vaddr: Value, acc_type: AccType) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ExclusiveReadMemory32,
            &[upper, vaddr, Value::ImmAccType(acc_type)],
        )
    }

    pub fn exclusive_read_memory_64(&mut self, vaddr: Value, acc_type: AccType) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ExclusiveReadMemory64,
            &[upper, vaddr, Value::ImmAccType(acc_type)],
        )
    }

    pub fn exclusive_read_memory_128(&mut self, vaddr: Value, acc_type: AccType) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ExclusiveReadMemory128,
            &[upper, vaddr, Value::ImmAccType(acc_type)],
        )
    }

    pub fn write_memory_8(&mut self, vaddr: Value, value: Value, acc_type: AccType) {
        let upper = self.imm_current_location_descriptor();
        let value = self.coerce_to_u8(value);
        self.emit_void(
            Opcode::A64WriteMemory8,
            &[upper, vaddr, value, Value::ImmAccType(acc_type)],
        );
    }

    pub fn write_memory_16(&mut self, vaddr: Value, value: Value, acc_type: AccType) {
        let upper = self.imm_current_location_descriptor();
        let value = self.coerce_to_u16(value);
        self.emit_void(
            Opcode::A64WriteMemory16,
            &[upper, vaddr, value, Value::ImmAccType(acc_type)],
        );
    }

    pub fn write_memory_32(&mut self, vaddr: Value, value: Value, acc_type: AccType) {
        let upper = self.imm_current_location_descriptor();
        self.emit_void(
            Opcode::A64WriteMemory32,
            &[upper, vaddr, value, Value::ImmAccType(acc_type)],
        );
    }

    pub fn write_memory_64(&mut self, vaddr: Value, value: Value, acc_type: AccType) {
        let upper = self.imm_current_location_descriptor();
        self.emit_void(
            Opcode::A64WriteMemory64,
            &[upper, vaddr, value, Value::ImmAccType(acc_type)],
        );
    }

    pub fn write_memory_128(&mut self, vaddr: Value, value: Value, acc_type: AccType) {
        let upper = self.imm_current_location_descriptor();
        self.emit_void(
            Opcode::A64WriteMemory128,
            &[upper, vaddr, value, Value::ImmAccType(acc_type)],
        );
    }

    pub fn exclusive_write_memory_8(
        &mut self,
        vaddr: Value,
        value: Value,
        acc_type: AccType,
    ) -> Value {
        let upper = self.imm_current_location_descriptor();
        let value = self.coerce_to_u8(value);
        self.emit(
            Opcode::A64ExclusiveWriteMemory8,
            &[upper, vaddr, value, Value::ImmAccType(acc_type)],
        )
    }

    pub fn exclusive_write_memory_16(
        &mut self,
        vaddr: Value,
        value: Value,
        acc_type: AccType,
    ) -> Value {
        let upper = self.imm_current_location_descriptor();
        let value = self.coerce_to_u16(value);
        self.emit(
            Opcode::A64ExclusiveWriteMemory16,
            &[upper, vaddr, value, Value::ImmAccType(acc_type)],
        )
    }

    pub fn exclusive_write_memory_32(
        &mut self,
        vaddr: Value,
        value: Value,
        acc_type: AccType,
    ) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ExclusiveWriteMemory32,
            &[upper, vaddr, value, Value::ImmAccType(acc_type)],
        )
    }

    pub fn exclusive_write_memory_64(
        &mut self,
        vaddr: Value,
        value: Value,
        acc_type: AccType,
    ) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ExclusiveWriteMemory64,
            &[upper, vaddr, value, Value::ImmAccType(acc_type)],
        )
    }

    pub fn exclusive_write_memory_128(
        &mut self,
        vaddr: Value,
        value: Value,
        acc_type: AccType,
    ) -> Value {
        let upper = self.imm_current_location_descriptor();
        self.emit(
            Opcode::A64ExclusiveWriteMemory128,
            &[upper, vaddr, value, Value::ImmAccType(acc_type)],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::block::Block;
    use crate::ir::value::InstRef;

    #[test]
    fn test_a64_emitter_x1_eq_x2_plus_x3() {
        let loc = A64LocationDescriptor::new(0x1000, 0, false);
        let mut block = Block::new(loc.to_location());
        {
            let mut e = A64IREmitter::with_location(&mut block, loc);

            // X2 = GetX(R2)
            let x2 = e.get_x(Reg::R2);
            // X3 = GetX(R3)
            let x3 = e.get_x(Reg::R3);
            // result = Add64(X2, X3, carry=0)
            let carry = e.ir().imm1(false);
            let result = e.ir().add_64(x2, x3, carry);
            // SetX(R1, result)
            e.set_x(Reg::R1, result);

            e.set_term(Terminal::ReturnToDispatch);
        }

        assert_eq!(block.inst_count(), 4);
        assert_eq!(block.get(InstRef(0)).opcode, Opcode::A64GetX);
        assert_eq!(block.get(InstRef(1)).opcode, Opcode::A64GetX);
        assert_eq!(block.get(InstRef(2)).opcode, Opcode::Add64);
        assert_eq!(block.get(InstRef(3)).opcode, Opcode::A64SetX);

        // Check use counts
        assert_eq!(block.get(InstRef(0)).use_count, 1); // x2 used by add
        assert_eq!(block.get(InstRef(1)).use_count, 1); // x3 used by add
        assert_eq!(block.get(InstRef(2)).use_count, 1); // result used by set_x

        assert!(!block.terminal.is_invalid());
    }

    #[test]
    fn test_a64_emitter_memory() {
        let loc = A64LocationDescriptor::new(0x2000, 0, false);
        let mut block = Block::new(loc.to_location());
        {
            let mut e = A64IREmitter::with_location(&mut block, loc);
            let addr = e.get_x(Reg::R0);
            let val = e.read_memory_32(addr, AccType::Normal);
            e.set_w(Reg::R1, val);
        }
        assert_eq!(block.inst_count(), 3);
        assert_eq!(block.get(InstRef(1)).opcode, Opcode::A64ReadMemory32);
    }

    #[test]
    fn test_a64_emitter_flags() {
        let loc = A64LocationDescriptor::new(0x3000, 0, false);
        let mut block = Block::new(loc.to_location());
        {
            let mut e = A64IREmitter::with_location(&mut block, loc);
            let x1 = e.get_x(Reg::R1);
            let x2 = e.get_x(Reg::R2);
            let carry = e.ir().imm1(true);
            let result = e.ir().add_64(x1, x2, carry);
            let nzcv = e.ir().get_nzcv_from_op(result);
            e.set_nzcv(nzcv);
        }
        assert_eq!(block.inst_count(), 5);
    }
}

use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::{AccType, Reg};

impl<'a> TranslatorVisitor<'a> {
    /// STXR - Store exclusive register
    pub fn stxr(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        let rs = Reg::from_u32(inst.rs());
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());

        let datasize = 8usize << (size as usize);
        let address = self.base_address(rn);
        let data = self.x(datasize.min(64), rt);

        let status = match datasize / 8 {
            1 => self
                .ir
                .exclusive_write_memory_8(address, data, AccType::Atomic),
            2 => self
                .ir
                .exclusive_write_memory_16(address, data, AccType::Atomic),
            4 => self
                .ir
                .exclusive_write_memory_32(address, data, AccType::Atomic),
            8 => self
                .ir
                .exclusive_write_memory_64(address, data, AccType::Atomic),
            _ => return self.interpret_this_instruction(),
        };
        self.set_x(32, rs, status);
        true
    }

    /// STLXR - Store-release exclusive register
    pub fn stlxr(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        let rs = Reg::from_u32(inst.rs());
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());

        let datasize = 8usize << (size as usize);
        let address = self.base_address(rn);
        let data = self.x(datasize.min(64), rt);

        let status = match datasize / 8 {
            1 => self
                .ir
                .exclusive_write_memory_8(address, data, AccType::Ordered),
            2 => self
                .ir
                .exclusive_write_memory_16(address, data, AccType::Ordered),
            4 => self
                .ir
                .exclusive_write_memory_32(address, data, AccType::Ordered),
            8 => self
                .ir
                .exclusive_write_memory_64(address, data, AccType::Ordered),
            _ => return self.interpret_this_instruction(),
        };
        self.set_x(32, rs, status);
        true
    }

    /// LDXR - Load exclusive register
    pub fn ldxr(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());

        let datasize = 8usize << (size as usize);
        let regsize = if size == 3 { 64 } else { 32 };

        let address = self.base_address(rn);
        let data = match datasize / 8 {
            1 => self.ir.exclusive_read_memory_8(address, AccType::Atomic),
            2 => self.ir.exclusive_read_memory_16(address, AccType::Atomic),
            4 => self.ir.exclusive_read_memory_32(address, AccType::Atomic),
            8 => self.ir.exclusive_read_memory_64(address, AccType::Atomic),
            _ => return self.interpret_this_instruction(),
        };

        let extended = self.sign_or_zero_extend(data, datasize, regsize, false);
        self.set_x(regsize, rt, extended);
        true
    }

    /// LDAXR - Load-acquire exclusive register
    pub fn ldaxr(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());

        let datasize = 8usize << (size as usize);
        let regsize = if size == 3 { 64 } else { 32 };

        let address = self.base_address(rn);
        let data = match datasize / 8 {
            1 => self.ir.exclusive_read_memory_8(address, AccType::Ordered),
            2 => self.ir.exclusive_read_memory_16(address, AccType::Ordered),
            4 => self.ir.exclusive_read_memory_32(address, AccType::Ordered),
            8 => self.ir.exclusive_read_memory_64(address, AccType::Ordered),
            _ => return self.interpret_this_instruction(),
        };

        let extended = self.sign_or_zero_extend(data, datasize, regsize, false);
        self.set_x(regsize, rt, extended);
        true
    }

    // ----- Pair exclusive operations -----
    //
    // Port of upstream `ExclusiveSharedDecodeAndOperation` (pair=true) in
    // load_store_exclusive.cpp. These were previously stubbed to the
    // interpreter (which is unimplemented → "Unimplemented instruction"),
    // crashing AArch64 titles that use 128-bit atomics (e.g. `ldxp x,x,[x]`
    // in allocators / lock-free code). `size` (= inst.size() = concat(1, sz))
    // is 2 (elsize 32) or 3 (elsize 64).

    /// Shared load-pair (LDXP / LDAXP).
    fn exclusive_load_pair(&mut self, inst: &DecodedInst, ordered: bool) -> bool {
        let size = inst.size();
        let elsize = 8usize << (size as usize);
        let rt2 = Reg::from_u32(inst.rt2());
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());
        let acctype = if ordered {
            AccType::Ordered
        } else {
            AccType::Atomic
        };

        // Constrained-unpredictable: load pair into the same register.
        if rt == rt2 {
            return self.unpredictable_instruction();
        }

        let address = self.base_address(rn);
        if elsize == 64 {
            let data = self.ir.exclusive_read_memory_128(address, acctype);
            let lo = self.ir.ir().vector_get_element(64, data, 0);
            let hi = self.ir.ir().vector_get_element(64, data, 1);
            self.set_x(64, rt, lo);
            self.set_x(64, rt2, hi);
        } else {
            let data = self.ir.exclusive_read_memory_64(address, acctype);
            let lo = self.ir.ir().least_significant_word(data);
            let hi = self.ir.ir().most_significant_word(data).result;
            self.set_x(32, rt, lo);
            self.set_x(32, rt2, hi);
        }
        true
    }

    /// Shared store-pair (STXP / STLXP).
    fn exclusive_store_pair(&mut self, inst: &DecodedInst, ordered: bool) -> bool {
        let size = inst.size();
        let elsize = 8usize << (size as usize);
        let rs = Reg::from_u32(inst.rs());
        let rt2 = Reg::from_u32(inst.rt2());
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());
        let acctype = if ordered {
            AccType::Ordered
        } else {
            AccType::Atomic
        };

        // Constrained-unpredictable: status reg aliases a data/base reg.
        if rs == rt || rs == rt2 || (rs == rn && rn != Reg::R31) {
            return self.unpredictable_instruction();
        }

        let address = self.base_address(rn);
        let status = if elsize == 64 {
            let a = self.x(64, rt);
            let b = self.x(64, rt2);
            let data = self.ir.ir().pack_2x64_to_1x128(a, b);
            self.ir.exclusive_write_memory_128(address, data, acctype)
        } else {
            let a = self.x(32, rt);
            let b = self.x(32, rt2);
            let data = self.ir.ir().pack_2x32_to_1x64(a, b);
            self.ir.exclusive_write_memory_64(address, data, acctype)
        };
        self.set_x(32, rs, status);
        true
    }

    /// STXP - Store exclusive pair.
    pub fn stxp(&mut self, inst: &DecodedInst) -> bool {
        self.exclusive_store_pair(inst, false)
    }
    /// STLXP - Store-release exclusive pair.
    pub fn stlxp(&mut self, inst: &DecodedInst) -> bool {
        self.exclusive_store_pair(inst, true)
    }
    /// LDXP - Load exclusive pair.
    pub fn ldxp(&mut self, inst: &DecodedInst) -> bool {
        self.exclusive_load_pair(inst, false)
    }
    /// LDAXP - Load-acquire exclusive pair.
    pub fn ldaxp(&mut self, inst: &DecodedInst) -> bool {
        self.exclusive_load_pair(inst, true)
    }

    /// STLR - Store-release register
    pub fn stlr(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());

        let datasize = 8usize << (size as usize);
        let address = self.base_address(rn);
        let data = self.x(datasize.min(64), rt);
        self.mem_write(address, data, datasize / 8, AccType::Ordered);
        true
    }

    /// LDAR - Load-acquire register
    pub fn ldar(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());

        let datasize = 8usize << (size as usize);
        let regsize = if size == 3 { 64 } else { 32 };
        let address = self.base_address(rn);
        let data = self.mem_read(address, datasize / 8, AccType::Ordered);
        let extended = self.sign_or_zero_extend(data, datasize, regsize, false);
        self.set_x(regsize, rt, extended);
        true
    }

    /// STLLR - Store LORelease register
    pub fn stllr(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());

        let datasize = 8usize << (size as usize);
        let address = self.base_address(rn);
        let data = self.x(datasize.min(64), rt);
        self.mem_write(address, data, datasize / 8, AccType::LimitedOrdered);
        true
    }

    /// LDLAR - Load LOAcquire register
    pub fn ldlar(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());

        let datasize = 8usize << (size as usize);
        let regsize = if size == 3 { 64 } else { 32 };
        let address = self.base_address(rn);
        let data = self.mem_read(address, datasize / 8, AccType::LimitedOrdered);
        let extended = self.sign_or_zero_extend(data, datasize, regsize, false);
        self.set_x(regsize, rt, extended);
        true
    }
}

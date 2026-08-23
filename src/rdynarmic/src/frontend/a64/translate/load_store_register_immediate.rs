use crate::frontend::a64::decoder::DecodedInst;
use crate::frontend::a64::translate::visitor::TranslatorVisitor;
use crate::frontend::a64::types::{AccType, Reg, Vec};
use crate::ir::emitter::MemOp;

impl<'a> TranslatorVisitor<'a> {
    fn simd_scale(&mut self, inst: &DecodedInst) -> Option<usize> {
        let scale = ((inst.bit(23) as usize) << 2) | inst.size() as usize;
        if scale > 4 {
            return None;
        }
        Some(scale)
    }

    fn zero_extend_to_quad_from_size(
        &mut self,
        datasize: usize,
        value: crate::ir::value::Value,
    ) -> crate::ir::value::Value {
        match datasize {
            8 => {
                let value = self.ir.ir().zero_extend_byte_to_long(value);
                self.ir.ir().zero_extend_to_quad(value)
            }
            16 => {
                let value = self.ir.ir().zero_extend_half_to_long(value);
                self.ir.ir().zero_extend_to_quad(value)
            }
            32 => {
                let value = self.ir.ir().zero_extend_word_to_long(value);
                self.ir.ir().zero_extend_to_quad(value)
            }
            64 => self.ir.ir().zero_extend_to_quad(value),
            128 => value,
            _ => panic!("Invalid FP/SIMD datasize {}", datasize),
        }
    }

    fn load_store_simd(
        &mut self,
        wback: bool,
        postindex: bool,
        scale: usize,
        offset_value: u64,
        memop: MemOp,
        rn: Reg,
        vt: Vec,
    ) -> bool {
        let datasize = 8usize << scale;
        let mut address = self.base_address(rn);

        if !postindex {
            let offset = self.ir.ir().imm64(offset_value);
            address = self.addr_add(address, offset);
        }

        match memop {
            MemOp::Store => {
                if datasize == 128 {
                    let data = self.ir.get_q(vt);
                    self.mem_write(address, data, 16, AccType::Vec);
                } else {
                    let vector = self.ir.get_q(vt);
                    let data = self.ir.ir().vector_get_element(datasize, vector, 0);
                    self.mem_write(address, data, datasize / 8, AccType::Vec);
                }
            }
            MemOp::Load => {
                let data = self.mem_read(address, datasize / 8, AccType::Vec);
                if datasize == 128 {
                    self.ir.set_q(vt, data);
                } else {
                    let data = self.zero_extend_to_quad_from_size(datasize, data);
                    self.ir.set_q(vt, data);
                }
            }
            MemOp::Prefetch => unreachable!(),
        }

        if wback {
            if postindex {
                let offset = self.ir.ir().imm64(offset_value);
                address = self.addr_add(address, offset);
            }
            self.writeback_address(rn, address);
        }

        true
    }

    /// STRx/LDRx (immediate) - Pre/post-index with 9-bit signed offset
    pub fn strx_ldrx_imm_1(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        // ARM STR/LDR (immediate) puts opc at bits[23:22] (00=STR, 01=LDR,
        // 10=LDRSW, 11=size-dependent signed). NOT bits[30:29] (which is part
        // of the size field). Same fix family as strx_reg/ldrx_reg.
        let opc = inst.bits(23, 22);
        let imm9 = inst.imm9_sext();
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());
        let not_postindex = inst.bit(11);

        let scale = size as usize;
        let datasize = 8usize << scale;

        // Prefetch (size=3, opc=2): NOP
        if size == 3 && opc == 2 {
            return true;
        }

        let is_store = opc == 0;
        let is_signed = (opc & 2) != 0;
        let regsize = if is_signed {
            if (opc & 1) != 0 {
                32
            } else {
                64
            }
        } else if size == 3 {
            64
        } else {
            datasize.max(32)
        };

        let base = self.base_address(rn);
        let offset = self.ir.ir().imm64(imm9 as u64);

        let address = if not_postindex {
            self.addr_add(base, offset)
        } else {
            base
        };

        if is_store {
            // Source register is W (for size 0-2 = 8/16/32 bit data) or X (for
            // size 3 = 64-bit). Read at regsize, then write only `datasize` bytes.
            let read_size = if size == 3 { 64 } else { 32 };
            let data = self.x(read_size, rt);
            self.mem_write(address, data, datasize / 8, AccType::Normal);
        } else {
            let data = self.mem_read(address, datasize / 8, AccType::Normal);
            let extended = self.sign_or_zero_extend(data, datasize, regsize, is_signed);
            self.set_x(regsize, rt, extended);
        }

        // Writeback
        let wb_addr = if not_postindex {
            address
        } else {
            self.addr_add(address, offset)
        };
        self.writeback_address(rn, wb_addr);

        true
    }

    /// STRx/LDRx (immediate) - Unsigned 12-bit offset, no writeback
    pub fn strx_ldrx_imm_2(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        // opc at bits[23:22] (see strx_ldrx_imm_1 for details).
        let opc = inst.bits(23, 22);
        let imm12 = inst.imm12();
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());

        let scale = size as usize;
        let datasize = 8usize << scale;

        if size == 3 && opc == 2 {
            return true;
        }

        let is_store = opc == 0;
        let is_signed = (opc & 2) != 0;
        let regsize = if is_signed {
            if (opc & 1) != 0 {
                32
            } else {
                64
            }
        } else if size == 3 {
            64
        } else {
            datasize.max(32)
        };

        let offset_val = (imm12 as u64) << scale;
        let base = self.base_address(rn);
        let offset = self.ir.ir().imm64(offset_val);
        let address = self.addr_add(base, offset);

        if is_store {
            // Source register is W (for size 0-2 = 8/16/32 bit data) or X (for
            // size 3 = 64-bit). Read at regsize, then write only `datasize` bytes.
            let read_size = if size == 3 { 64 } else { 32 };
            let data = self.x(read_size, rt);
            self.mem_write(address, data, datasize / 8, AccType::Normal);
        } else {
            let data = self.mem_read(address, datasize / 8, AccType::Normal);
            let extended = self.sign_or_zero_extend(data, datasize, regsize, is_signed);
            self.set_x(regsize, rt, extended);
        }

        true
    }

    /// STURx/LDURx - Unscaled immediate offset
    pub fn sturx_ldurx(&mut self, inst: &DecodedInst) -> bool {
        let size = inst.size();
        // opc at bits[23:22] (see strx_ldrx_imm_1 for details).
        let opc = inst.bits(23, 22);
        let imm9 = inst.imm9_sext();
        let rn = Reg::from_u32(inst.rn());
        let rt = Reg::from_u32(inst.rd());

        let scale = size as usize;
        let datasize = 8usize << scale;

        if size == 3 && opc == 2 {
            return true;
        }

        let is_store = opc == 0;
        let is_signed = (opc & 2) != 0;
        let regsize = if is_signed {
            if (opc & 1) != 0 {
                32
            } else {
                64
            }
        } else if size == 3 {
            64
        } else {
            datasize.max(32)
        };

        let base = self.base_address(rn);
        let offset = self.ir.ir().imm64(imm9 as u64);
        let address = self.addr_add(base, offset);

        if is_store {
            // Source register is W (for size 0-2 = 8/16/32 bit data) or X (for
            // size 3 = 64-bit). Read at regsize, then write only `datasize` bytes.
            let read_size = if size == 3 { 64 } else { 32 };
            let data = self.x(read_size, rt);
            self.mem_write(address, data, datasize / 8, AccType::Normal);
        } else {
            let data = self.mem_read(address, datasize / 8, AccType::Normal);
            let extended = self.sign_or_zero_extend(data, datasize, regsize, is_signed);
            self.set_x(regsize, rt, extended);
        }

        true
    }

    /// STR (immediate, SIMD&FP) - Pre/post-index 9-bit offset
    pub fn str_imm_fpsimd_1(&mut self, inst: &DecodedInst) -> bool {
        let Some(scale) = self.simd_scale(inst) else {
            return self.unallocated_encoding();
        };
        let imm9 = inst.imm9_sext();
        let rn = Reg::from_u32(inst.rn());
        let rt = Vec::from_u32(inst.rd());
        let not_postindex = inst.bit(11);
        self.load_store_simd(
            true,
            !not_postindex,
            scale,
            imm9 as u64,
            MemOp::Store,
            rn,
            rt,
        )
    }

    /// STR (immediate, SIMD&FP) - Unsigned 12-bit offset
    pub fn str_imm_fpsimd_2(&mut self, inst: &DecodedInst) -> bool {
        let Some(scale) = self.simd_scale(inst) else {
            return self.unallocated_encoding();
        };
        let imm12 = inst.imm12();
        let rn = Reg::from_u32(inst.rn());
        let rt = Vec::from_u32(inst.rd());
        let offset_val = (imm12 as u64) << scale;
        self.load_store_simd(false, false, scale, offset_val, MemOp::Store, rn, rt)
    }

    /// LDR (immediate, SIMD&FP) - Pre/post-index 9-bit offset
    pub fn ldr_imm_fpsimd_1(&mut self, inst: &DecodedInst) -> bool {
        let Some(scale) = self.simd_scale(inst) else {
            return self.unallocated_encoding();
        };
        let imm9 = inst.imm9_sext();
        let rn = Reg::from_u32(inst.rn());
        let rt = Vec::from_u32(inst.rd());
        let not_postindex = inst.bit(11);
        self.load_store_simd(
            true,
            !not_postindex,
            scale,
            imm9 as u64,
            MemOp::Load,
            rn,
            rt,
        )
    }

    /// LDR (immediate, SIMD&FP) - Unsigned 12-bit offset
    pub fn ldr_imm_fpsimd_2(&mut self, inst: &DecodedInst) -> bool {
        let Some(scale) = self.simd_scale(inst) else {
            return self.unallocated_encoding();
        };
        let imm12 = inst.imm12();
        let rn = Reg::from_u32(inst.rn());
        let rt = Vec::from_u32(inst.rd());
        let offset_val = (imm12 as u64) << scale;
        self.load_store_simd(false, false, scale, offset_val, MemOp::Load, rn, rt)
    }

    /// STUR (SIMD&FP) - Unscaled immediate
    pub fn stur_fpsimd(&mut self, inst: &DecodedInst) -> bool {
        let Some(scale) = self.simd_scale(inst) else {
            return self.unallocated_encoding();
        };
        let imm9 = inst.imm9_sext();
        let rn = Reg::from_u32(inst.rn());
        let rt = Vec::from_u32(inst.rd());
        self.load_store_simd(false, false, scale, imm9 as u64, MemOp::Store, rn, rt)
    }

    /// LDUR (SIMD&FP) - Unscaled immediate
    pub fn ldur_fpsimd(&mut self, inst: &DecodedInst) -> bool {
        let Some(scale) = self.simd_scale(inst) else {
            return self.unallocated_encoding();
        };
        let imm9 = inst.imm9_sext();
        let rn = Reg::from_u32(inst.rn());
        let rt = Vec::from_u32(inst.rd());
        self.load_store_simd(false, false, scale, imm9 as u64, MemOp::Load, rn, rt)
    }

    // --- PRFM instructions (prefetch hints - treated as NOP) ---
    pub fn prfm_imm(&mut self, _inst: &DecodedInst) -> bool {
        true
    }
    pub fn prfm_lit(&mut self, _inst: &DecodedInst) -> bool {
        true
    }
    pub fn prfm_unscaled_imm(&mut self, _inst: &DecodedInst) -> bool {
        true
    }
}

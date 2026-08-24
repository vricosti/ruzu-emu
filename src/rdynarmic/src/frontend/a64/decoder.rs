// Include the generated decoder tables and decode function
include!(concat!(env!("OUT_DIR"), "/a64_decoder_gen.rs"));

/// Helper functions for extracting fields from decoded instructions.
impl DecodedInst {
    /// Extract a range of bits from the raw instruction.
    pub fn bits(&self, hi: u32, lo: u32) -> u32 {
        let width = hi - lo + 1;
        let mask = (1u32 << width) - 1;
        (self.raw >> lo) & mask
    }

    /// Extract a single bit.
    pub fn bit(&self, pos: u32) -> bool {
        (self.raw >> pos) & 1 != 0
    }

    /// Rd field (bits [4:0]).
    pub fn rd(&self) -> u32 {
        self.bits(4, 0)
    }

    /// Rn field (bits [9:5]).
    pub fn rn(&self) -> u32 {
        self.bits(9, 5)
    }

    /// Rm field (bits [20:16]).
    pub fn rm(&self) -> u32 {
        self.bits(20, 16)
    }

    /// Ra field (bits [14:10]).
    pub fn ra(&self) -> u32 {
        self.bits(14, 10)
    }

    /// sf/size flag (bit 31) — 1 for 64-bit, 0 for 32-bit.
    pub fn sf(&self) -> bool {
        self.bit(31)
    }

    /// Operand size: 32 if sf=0, 64 if sf=1.
    pub fn datasize(&self) -> usize {
        if self.sf() {
            64
        } else {
            32
        }
    }

    /// N bit (bit 22) for logical immediate.
    pub fn n(&self) -> bool {
        self.bit(22)
    }

    /// imms field (bits [15:10]).
    pub fn imms(&self) -> u32 {
        self.bits(15, 10)
    }

    /// immr field (bits [21:16]).
    pub fn immr(&self) -> u32 {
        self.bits(21, 16)
    }

    /// imm12 field (bits [21:10]).
    pub fn imm12(&self) -> u32 {
        self.bits(21, 10)
    }

    /// imm16 field (bits [20:5]).
    pub fn imm16(&self) -> u32 {
        self.bits(20, 5)
    }

    /// imm19 field (bits [23:5]), sign-extended.
    pub fn imm19_sext(&self) -> i64 {
        let imm = self.bits(23, 5);
        ((imm as i32) << 13 >> 13) as i64
    }

    /// imm26 field (bits [25:0]), sign-extended.
    pub fn imm26_sext(&self) -> i64 {
        let imm = self.bits(25, 0);
        ((imm as i32) << 6 >> 6) as i64
    }

    /// hw (shift) field (bits [22:21]).
    pub fn hw(&self) -> u32 {
        self.bits(22, 21)
    }

    /// shift field (bits [23:22]).
    pub fn shift(&self) -> u32 {
        self.bits(23, 22)
    }

    /// option field (bits [15:13]).
    pub fn option(&self) -> u32 {
        self.bits(15, 13)
    }

    /// cond field (bits [15:12]).
    pub fn cond_field(&self) -> u32 {
        self.bits(15, 12)
    }

    /// opc field (bits [30:29]).
    pub fn opc(&self) -> u32 {
        self.bits(30, 29)
    }

    /// size field (bits [31:30]).
    pub fn size(&self) -> u32 {
        self.bits(31, 30)
    }

    /// Q bit (bit 30) for SIMD.
    pub fn q(&self) -> bool {
        self.bit(30)
    }

    /// imm9 field (bits [20:12]), sign-extended.
    pub fn imm9_sext(&self) -> i64 {
        let imm = self.bits(20, 12);
        ((imm as i32) << 23 >> 23) as i64
    }

    /// imm7 field (bits [21:15]), sign-extended.
    pub fn imm7_sext(&self) -> i64 {
        let imm = self.bits(21, 15);
        ((imm as i32) << 25 >> 25) as i64
    }

    /// Rs field (bits [20:16]) for store exclusives.
    pub fn rs(&self) -> u32 {
        self.bits(20, 16)
    }

    /// Rt2/Ru field (bits [14:10]).
    pub fn rt2(&self) -> u32 {
        self.bits(14, 10)
    }
}

#[cfg(test)]
mod system_flag_decoder_tests {
    use super::{decode, A64InstructionName};

    #[test]
    fn patterns_with_trailing_architecture_comments_are_generated() {
        let cases = [
            (0xD500_401F, A64InstructionName::CFINV),
            (0xBA02_8465, A64InstructionName::RMIF),
            (0xD500_403F, A64InstructionName::XAFlag),
            (0xD500_405F, A64InstructionName::AXFlag),
        ];

        for (encoding, expected_name) in cases {
            let decoded = decode(encoding).expect("system-flag instruction should decode");
            assert_eq!(decoded.name, expected_name, "encoding 0x{encoding:08X}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_movz() {
        // MOVZ X0, #1 => 0xD2800020
        let result = decode(0xD2800020);
        assert!(result.is_some(), "Failed to decode MOVZ X0, #1");
        let inst = result.unwrap();
        assert_eq!(inst.name, A64InstructionName::MOVZ);
        assert_eq!(inst.rd(), 0); // X0
        assert_eq!(inst.imm16(), 1);
    }

    #[test]
    fn test_decode_add_imm() {
        // ADD X1, X2, #5 => sf=1, op=0, S=0, shift=00, imm12=5, Rn=2, Rd=1
        // 1_00_10001_00_000000000101_00010_00001 = 0x91001441
        let result = decode(0x91001441);
        assert!(result.is_some(), "Failed to decode ADD X1, X2, #5");
        let inst = result.unwrap();
        assert_eq!(inst.name, A64InstructionName::ADD_imm);
        assert_eq!(inst.rd(), 1);
        assert_eq!(inst.rn(), 2);
        assert_eq!(inst.imm12(), 5);
    }

    #[test]
    fn test_decode_b_uncond() {
        // B #4 => 0x14000001
        let result = decode(0x14000001);
        assert!(result.is_some(), "Failed to decode B #4");
        let inst = result.unwrap();
        assert_eq!(inst.name, A64InstructionName::B_uncond);
    }

    #[test]
    fn test_decode_svc() {
        // SVC #0 => 0xD4000001
        let result = decode(0xD4000001);
        assert!(result.is_some(), "Failed to decode SVC #0");
        let inst = result.unwrap();
        assert_eq!(inst.name, A64InstructionName::SVC);
    }

    #[test]
    fn test_decode_hvc_is_not_exposed() {
        // HVC #0 => 0xD4000002
        // Upstream A64 decoder leaves HVC commented out in a64.inc.
        assert!(decode(0xD4000002).is_none());
    }

    #[test]
    fn test_decode_smc_is_not_exposed() {
        // SMC #0 => 0xD4000003
        // Upstream A64 decoder leaves SMC commented out in a64.inc.
        assert!(decode(0xD4000003).is_none());
    }

    #[test]
    fn test_decode_ret() {
        // RET (X30) => 0xD65F03C0
        let result = decode(0xD65F03C0);
        assert!(result.is_some(), "Failed to decode RET");
        let inst = result.unwrap();
        assert_eq!(inst.name, A64InstructionName::RET);
        assert_eq!(inst.rn(), 30); // X30/LR
    }

    #[test]
    fn test_decode_nop() {
        // NOP => 0xD503201F
        let result = decode(0xD503201F);
        assert!(result.is_some(), "Failed to decode NOP");
        let inst = result.unwrap();
        assert_eq!(inst.name, A64InstructionName::NOP);
    }

    #[test]
    fn test_decode_unknown() {
        // 0x00000000 is a reserved encoding
        // It may or may not decode - just verify no panic
        let _ = decode(0x00000000);
    }

    #[test]
    fn test_decode_bl() {
        // BL #0 => 0x94000000
        let result = decode(0x94000000);
        assert!(result.is_some(), "Failed to decode BL");
        let inst = result.unwrap();
        assert_eq!(inst.name, A64InstructionName::BL);
    }

    #[test]
    fn test_decode_br() {
        // BR X8 => 0xD61F0100
        let result = decode(0xD61F0100);
        assert!(result.is_some(), "Failed to decode BR X8");
        let inst = result.unwrap();
        assert_eq!(inst.name, A64InstructionName::BR);
        assert_eq!(inst.rn(), 8);
    }
}

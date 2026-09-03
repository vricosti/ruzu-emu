// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Register-or-memory operand wrapper.
//!
//! This is the Rust counterpart of Dynarmic's `backend/x64/oparg.h`.

use rxbyak::{Address, Reg, RegMem, RegMemImm};

#[derive(Clone, Copy, Debug, Default)]
pub struct OpArg {
    inner: Option<RegMem>,
}

impl OpArg {
    pub fn operand(self) -> RegMem {
        self.inner.expect("uninitialized OpArg")
    }

    pub fn set_bit(&mut self, bits: u16) {
        self.inner = Some(match self.operand() {
            RegMem::Reg(reg) => RegMem::Reg(reg.change_bit(bits).expect("invalid OpArg bit width")),
            RegMem::Mem(address) => RegMem::Mem(address.change_bit(bits)),
        });
    }
}

impl From<Address> for OpArg {
    fn from(address: Address) -> Self {
        Self {
            inner: Some(RegMem::Mem(address)),
        }
    }
}

impl From<Reg> for OpArg {
    fn from(reg: Reg) -> Self {
        Self {
            inner: Some(RegMem::Reg(reg)),
        }
    }
}

impl From<OpArg> for RegMem {
    fn from(op_arg: OpArg) -> Self {
        op_arg.operand()
    }
}

impl From<OpArg> for RegMemImm {
    fn from(op_arg: OpArg) -> Self {
        RegMemImm::from(op_arg.operand())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxbyak::{ptr, RegExp, RAX};

    #[test]
    fn register_operand_changes_to_each_upstream_width() {
        for bits in [8, 16, 32, 64] {
            let mut op_arg = OpArg::from(RAX);
            op_arg.set_bit(bits);
            let reg = op_arg.operand().as_reg().copied().expect("register");
            assert_eq!(reg.index(), RAX.index());
            assert_eq!(reg.bit_width(), bits);
        }
    }

    #[test]
    fn address_operand_changes_size_without_changing_expression() {
        let address = ptr(RegExp::from(RAX) + 24);
        let mut op_arg = OpArg::from(address);
        op_arg.set_bit(32);
        let changed = op_arg.operand();
        let changed = changed.as_mem().expect("memory");
        assert_eq!(changed.bit_width(), 32);
        assert_eq!(changed.register_expression(), address.register_expression());
    }

    #[test]
    #[should_panic(expected = "uninitialized OpArg")]
    fn default_operand_matches_upstream_unreachable_state() {
        let _ = OpArg::default().operand();
    }
}

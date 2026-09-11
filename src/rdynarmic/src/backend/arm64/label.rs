//! Minimal AArch64 label support for backend-local branch patching.
//!
//! Upstream uses Oaknut labels. This keeps the same ownership model without
//! exposing a scheduler or dispatcher abstraction through the emitter.

use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::inst;
use crate::ir::cond::Cond;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingBranch {
    Uncond { offset: usize },
    Cond { offset: usize, cond: Cond },
    CbzX { offset: usize, rt: u8 },
    CbnzX { offset: usize, rt: u8 },
    TbnzX { offset: usize, rt: u8, bit: u8 },
}

#[derive(Default, Debug)]
pub struct Label {
    offset: Option<usize>,
    pending: Vec<PendingBranch>,
}

impl Label {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind(&mut self, code: &mut BlockOfCode) -> Result<(), String> {
        let target_offset = code.code_size();
        if self.offset.replace(target_offset).is_some() {
            return Err("ARM64 label bound more than once".to_string());
        }

        for branch in self.pending.drain(..) {
            match branch {
                PendingBranch::Uncond { offset } => {
                    let pc_offset = branch_pc_offset_isize(offset, target_offset)?;
                    code.patch_u32(offset, inst::b_imm(pc_offset))?;
                }
                PendingBranch::Cond { offset, cond } => {
                    let pc_offset = branch_pc_offset(offset, target_offset)?;
                    code.patch_u32(offset, inst::b_cond(cond, pc_offset))?;
                }
                PendingBranch::CbzX { offset, rt } => {
                    let pc_offset = branch_pc_offset(offset, target_offset)?;
                    code.patch_u32(offset, inst::cbz_x(rt, pc_offset))?;
                }
                PendingBranch::CbnzX { offset, rt } => {
                    let pc_offset = branch_pc_offset(offset, target_offset)?;
                    code.patch_u32(offset, inst::cbnz_x(rt, pc_offset))?;
                }
                PendingBranch::TbnzX { offset, rt, bit } => {
                    let pc_offset = branch_pc_offset(offset, target_offset)?;
                    code.patch_u32(offset, inst::tbnz_x(rt, bit, pc_offset))?;
                }
            }
        }
        Ok(())
    }

    pub fn b(&mut self, code: &mut BlockOfCode) -> Result<usize, String> {
        let offset = code.write_u32(inst::b_imm(0))?;
        if let Some(target_offset) = self.offset {
            let pc_offset = branch_pc_offset_isize(offset, target_offset)?;
            code.patch_u32(offset, inst::b_imm(pc_offset))?;
        } else {
            self.pending.push(PendingBranch::Uncond { offset });
        }
        Ok(offset)
    }

    pub fn b_cond(&mut self, code: &mut BlockOfCode, cond: Cond) -> Result<usize, String> {
        let offset = code.write_u32(inst::b_cond(cond, 0))?;
        if let Some(target_offset) = self.offset {
            let pc_offset = branch_pc_offset(offset, target_offset)?;
            code.patch_u32(offset, inst::b_cond(cond, pc_offset))?;
        } else {
            self.pending.push(PendingBranch::Cond { offset, cond });
        }
        Ok(offset)
    }

    pub fn cbz_x(&mut self, code: &mut BlockOfCode, rt: u8) -> Result<usize, String> {
        let offset = code.write_u32(inst::cbz_x(rt, 0))?;
        if let Some(target_offset) = self.offset {
            let pc_offset = branch_pc_offset(offset, target_offset)?;
            code.patch_u32(offset, inst::cbz_x(rt, pc_offset))?;
        } else {
            self.pending.push(PendingBranch::CbzX { offset, rt });
        }
        Ok(offset)
    }

    pub fn cbnz_x(&mut self, code: &mut BlockOfCode, rt: u8) -> Result<usize, String> {
        let offset = code.write_u32(inst::cbnz_x(rt, 0))?;
        if let Some(target_offset) = self.offset {
            let pc_offset = branch_pc_offset(offset, target_offset)?;
            code.patch_u32(offset, inst::cbnz_x(rt, pc_offset))?;
        } else {
            self.pending.push(PendingBranch::CbnzX { offset, rt });
        }
        Ok(offset)
    }

    /// `tbnz xT, #bit, label` (oaknut `TBNZ(XReg, imm, Label&)`).
    pub fn tbnz_x(&mut self, code: &mut BlockOfCode, rt: u8, bit: u8) -> Result<usize, String> {
        let offset = code.write_u32(inst::tbnz_x(rt, bit, 0))?;
        if let Some(target_offset) = self.offset {
            let pc_offset = branch_pc_offset(offset, target_offset)?;
            code.patch_u32(offset, inst::tbnz_x(rt, bit, pc_offset))?;
        } else {
            self.pending.push(PendingBranch::TbnzX { offset, rt, bit });
        }
        Ok(offset)
    }
}

fn branch_pc_offset(branch_offset: usize, target_offset: usize) -> Result<i32, String> {
    i32::try_from(target_offset as isize - branch_offset as isize)
        .map_err(|_| "ARM64 label branch offset overflow".to_string())
}

fn branch_pc_offset_isize(branch_offset: usize, target_offset: usize) -> Result<isize, String> {
    Ok(branch_pc_offset(branch_offset, target_offset)? as isize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emitted_words(code: &BlockOfCode) -> Vec<u32> {
        (0..code.code_size() / 4)
            .map(|index| unsafe {
                code.code_base_ptr()
                    .add(index * 4)
                    .cast::<u32>()
                    .read_unaligned()
            })
            .collect()
    }

    #[test]
    fn forward_conditional_branch_is_patched_on_bind() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut label = Label::new();

        label.b_cond(&mut code, Cond::EQ).unwrap();
        code.write_u32(inst::nop()).unwrap();
        label.bind(&mut code).unwrap();

        assert_eq!(
            emitted_words(&code),
            vec![inst::b_cond(Cond::EQ, 8), inst::nop()]
        );
    }

    #[test]
    fn already_bound_label_patches_branch_immediately() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut label = Label::new();

        label.bind(&mut code).unwrap();
        label.b_cond(&mut code, Cond::EQ).unwrap();

        assert_eq!(emitted_words(&code), vec![inst::b_cond(Cond::EQ, 0)]);
    }

    #[test]
    fn forward_unconditional_and_cbz_branches_are_patched_on_bind() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut label = Label::new();

        label.b(&mut code).unwrap();
        label.cbz_x(&mut code, 16).unwrap();
        label.cbnz_x(&mut code, 17).unwrap();
        code.write_u32(inst::nop()).unwrap();
        label.bind(&mut code).unwrap();

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::b_imm(16),
                inst::cbz_x(16, 12),
                inst::cbnz_x(17, 8),
                inst::nop()
            ]
        );
    }
}

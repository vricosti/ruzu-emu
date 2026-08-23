mod a64_translate;
mod branch;
mod data_processing_addsub;
mod data_processing_bitfield;
mod data_processing_ccmp;
mod data_processing_crc32;
mod data_processing_csel;
mod data_processing_logical;
mod data_processing_multiply;
mod data_processing_pcrel;
mod data_processing_register;
mod data_processing_shift;
mod exception;
mod floating_point_compare;
mod floating_point_conditional_compare;
mod floating_point_conditional_select;
mod floating_point_conversion_fixed_point;
mod floating_point_conversion_integer;
mod floating_point_data_processing_one_register;
mod floating_point_data_processing_three_register;
mod floating_point_data_processing_two_register;
mod helpers;
mod load_store_exclusive;
mod load_store_literal;
mod load_store_multiple_structures;
mod load_store_register_immediate;
mod load_store_register_offset;
mod load_store_register_pair;
mod load_store_single_structure;
mod load_store_unprivileged;
mod move_wide;
mod simd_across_lanes;
mod simd_aes;
mod simd_copy;
mod simd_crypto_four_register;
mod simd_crypto_three_register;
mod simd_extract;
mod simd_modified_immediate;
mod simd_permute;
mod simd_scalar_pairwise;
mod simd_scalar_shift_by_immediate;
mod simd_scalar_three_same;
mod simd_scalar_two_register_misc;
mod simd_scalar_x_indexed_element;
mod simd_sha;
mod simd_sha512;
mod simd_shift_by_immediate;
mod simd_table_lookup;
mod simd_three_different;
mod simd_three_same;
mod simd_three_same_extra;
mod simd_two_register_misc;
mod simd_vector_x_indexed_element;
mod sys_dc;
mod sys_ic;
mod system;
mod system_flag_format;
mod system_flag_manipulation;
mod visitor;

pub use a64_translate::{
    translate, translate_single_instruction, MemoryReadCodeFn, TranslationOptions,
};
pub use visitor::TranslatorVisitor;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::location::A64LocationDescriptor;
    use crate::ir::terminal::Terminal;

    #[test]
    fn test_translate_simple_add() {
        // ADD X1, X2, #5 => 0x91001441
        // RET => 0xD65F03C0
        let code: Vec<u32> = vec![0x91001441, 0xD65F03C0];
        let loc = A64LocationDescriptor::new(0x1000, 0, false);

        let block = translate(
            loc,
            &move |pc: u64| {
                let offset = ((pc - 0x1000) / 4) as usize;
                code.get(offset).copied()
            },
            TranslationOptions::default(),
        );

        assert!(block.inst_count() > 0);
        assert!(!block.terminal.is_invalid());
    }

    #[test]
    fn test_translate_branch() {
        // B #8 => 0x14000002
        let code: Vec<u32> = vec![0x14000002];
        let loc = A64LocationDescriptor::new(0x2000, 0, false);

        let block = translate(
            loc,
            &move |pc: u64| {
                let offset = ((pc - 0x2000) / 4) as usize;
                code.get(offset).copied()
            },
            TranslationOptions::default(),
        );

        assert!(!block.terminal.is_invalid());
    }

    #[test]
    fn test_translate_svc() {
        // SVC #0 => 0xD4000001
        let code: Vec<u32> = vec![0xD4000001];
        let loc = A64LocationDescriptor::new(0x3000, 0, false);

        let block = translate(
            loc,
            &move |pc: u64| {
                let offset = ((pc - 0x3000) / 4) as usize;
                code.get(offset).copied()
            },
            TranslationOptions::default(),
        );

        assert!(!block.terminal.is_invalid());
    }

    #[test]
    fn test_translate_mul_vec_emits_vector_multiply() {
        // MUL V26.4S, V26.4S, V28.4S; SVC #0.
        // nx-hbmenu hits this at PC 0x801866AC.
        let code: Vec<u32> = vec![0x4EBC_9F5A, 0xD400_0001];
        let loc = A64LocationDescriptor::new(0x8018_66AC, 0, false);

        let block = translate(
            loc,
            &move |pc: u64| {
                let offset = ((pc - 0x8018_66AC) / 4) as usize;
                code.get(offset).copied()
            },
            TranslationOptions::default(),
        );

        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, crate::ir::opcode::Opcode::VectorMultiply32)));
    }

    #[test]
    fn test_translate_mla_vec_emits_multiply_then_add() {
        // MLA V1.4S, V0.4S, V2.4S; SVC #0.
        let code: Vec<u32> = vec![0x4EA2_9401, 0xD400_0001];
        let loc = A64LocationDescriptor::new(0x821F_F88C, 0, false);

        let block = translate(
            loc,
            &move |pc: u64| {
                let offset = ((pc - 0x821F_F88C) / 4) as usize;
                code.get(offset).copied()
            },
            TranslationOptions::default(),
        );

        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, crate::ir::opcode::Opcode::VectorMultiply32)));
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, crate::ir::opcode::Opcode::VectorAdd32)));
    }

    #[test]
    fn test_translate_mls_vec_emits_multiply_then_subtract() {
        // MLS V1.4S, V0.4S, V2.4S; SVC #0.
        let code: Vec<u32> = vec![0x6EA2_9401, 0xD400_0001];
        let loc = A64LocationDescriptor::new(0x4000, 0, false);

        let block = translate(
            loc,
            &move |pc: u64| {
                let offset = ((pc - 0x4000) / 4) as usize;
                code.get(offset).copied()
            },
            TranslationOptions::default(),
        );

        assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, crate::ir::opcode::Opcode::VectorMultiply32)));
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, crate::ir::opcode::Opcode::VectorSub32)));
    }

    #[test]
    fn test_translate_high_narrowing_family() {
        let cases = [
            (0x0E21_4041, crate::ir::opcode::Opcode::VectorAdd16),
            (0x2E21_4041, crate::ir::opcode::Opcode::VectorAdd16),
            (0x0E21_6041, crate::ir::opcode::Opcode::VectorSub16),
            (0x2E21_6041, crate::ir::opcode::Opcode::VectorSub16),
        ];

        for (encoding, arithmetic_opcode) in cases {
            let code: Vec<u32> = vec![encoding, 0xD400_0001];
            let loc = A64LocationDescriptor::new(0x4000, 0, false);
            let block = translate(
                loc,
                &move |pc: u64| {
                    let offset = ((pc - 0x4000) / 4) as usize;
                    code.get(offset).copied()
                },
                TranslationOptions::default(),
            );

            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == arithmetic_opcode));
            assert!(block
                .instructions
                .iter()
                .any(|inst| matches!(inst.opcode, crate::ir::opcode::Opcode::VectorNarrow16)));
        }
    }

    #[test]
    fn test_translate_fp_paired_min_max_family() {
        let cases = [
            (0x2E26_C4A5, crate::ir::opcode::Opcode::FPMaxNumeric32),
            (0x2E26_F4A5, crate::ir::opcode::Opcode::FPMax32),
            (0x2EA6_C4A5, crate::ir::opcode::Opcode::FPMinNumeric32),
            (0x2EA6_F4A5, crate::ir::opcode::Opcode::FPMin32),
        ];

        for (encoding, fp_opcode) in cases {
            let code: Vec<u32> = vec![encoding, 0xD400_0001];
            let loc = A64LocationDescriptor::new(0x4000, 0, false);
            let block = translate(
                loc,
                &move |pc: u64| {
                    let offset = ((pc - 0x4000) / 4) as usize;
                    code.get(offset).copied()
                },
                TranslationOptions::default(),
            );

            assert!(!matches!(block.terminal, Terminal::Interpret { .. }));
            assert!(block
                .instructions
                .iter()
                .any(|inst| inst.opcode == fp_opcode));
        }
    }

    #[test]
    fn test_translate_str_imm_fpsimd_byte_extracts_lane_zero() {
        // STR B1, [X0]; SVC #0. Stores smaller than 128 bits must extract
        // lane 0 from Q1 before writing memory, matching upstream LoadStoreSIMD.
        let code: Vec<u32> = vec![0x3D00_0001, 0xD400_0001];
        let loc = A64LocationDescriptor::new(0x6000, 0, false);

        let block = translate(
            loc,
            &move |pc: u64| {
                let offset = ((pc - 0x6000) / 4) as usize;
                code.get(offset).copied()
            },
            TranslationOptions::default(),
        );

        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, crate::ir::opcode::Opcode::VectorGetElement8)));
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, crate::ir::opcode::Opcode::A64WriteMemory8)));
    }

    #[test]
    fn test_translate_udf_raises_exception_instead_of_interpret() {
        let code: Vec<u32> = vec![0x0000_0000];
        let loc = A64LocationDescriptor::new(0x5000, 0, false);

        let block = translate(
            loc,
            &move |pc: u64| {
                let offset = ((pc - 0x5000) / 4) as usize;
                code.get(offset).copied()
            },
            TranslationOptions::default(),
        );

        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { else_ } if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, crate::ir::opcode::Opcode::A64ExceptionRaised)));
    }

    #[test]
    fn test_translate_missing_code_raises_no_execute_fault() {
        let loc = A64LocationDescriptor::new(0x4000, 0, false);

        let block = translate(loc, &move |_pc: u64| None, TranslationOptions::default());

        assert!(matches!(
            &block.terminal,
            Terminal::CheckHalt { else_ } if matches!(else_.as_ref(), Terminal::ReturnToDispatch)
        ));
        assert!(block
            .instructions
            .iter()
            .any(|inst| matches!(inst.opcode, crate::ir::opcode::Opcode::A64ExceptionRaised)));
    }
}

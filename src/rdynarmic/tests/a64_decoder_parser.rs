#[path = "../a64_decoder_parser.rs"]
mod a64_decoder_parser;

use a64_decoder_parser::parse_inst_line;

#[test]
fn ignores_parentheses_in_display_name_and_trailing_comment() {
    let pattern = parse_inst_line(
        "INST(FMOV_3, \"FMOV (vector, immediate)\", \"0Q00111100000abc111111defghddddd\") // FMOV (alias)",
    )
    .expect("active instruction pattern should parse");

    assert_eq!(pattern.name, "FMOV_3");
    assert_eq!(pattern.display_name, "FMOV (vector, immediate)");
    assert_eq!(pattern.bitstring, "0Q00111100000abc111111defghddddd");
    assert_eq!(pattern.specificity, 18);
    assert_eq!(pattern.mask.count_ones(), pattern.specificity);
    assert_eq!(pattern.expect & !pattern.mask, 0);
}

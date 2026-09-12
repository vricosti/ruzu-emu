//! Keep machine encoding in rhazel, as upstream keeps it in Oaknut.

#[test]
fn arm64_emitters_share_the_typed_generator() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/backend/arm64");
    let mut checked = 0;
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if !name.starts_with("emit_arm64")
            && !matches!(
                name,
                "abi.rs" | "reg_alloc.rs" | "fpsr_manager.rs" | "prelude.rs"
            )
        {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        // Raw encoders remain legitimate independent expectations in unit tests.
        let production = source.split("mod tests {").next().unwrap();
        for forbidden in [
            ".write_u32(",
            ".patch_u32(",
            ".patch_u32_deferred_icache(",
            "&mut BlockOfCode",
            "CodeGenerator::new(code)",
        ] {
            assert!(!production.contains(forbidden), "{name}: {forbidden}");
        }
        checked += 1;
    }
    assert!(checked >= 19, "backend owner files were not inspected");
}

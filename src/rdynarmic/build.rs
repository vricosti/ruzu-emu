use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;

/// Represents a single instruction pattern from a64.inc
struct Pattern {
    name: String,
    #[allow(dead_code)]
    display_name: String,
    bitstring: String,
    mask: u32,
    expect: u32,
    specificity: u32, // number of fixed bits
}

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let inc_path = Path::new("src/frontend/a64/a64.inc");

    println!("cargo:rerun-if-changed={}", inc_path.display());

    let content = fs::read_to_string(inc_path).expect("Failed to read a64.inc");
    let mut patterns = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if !line.starts_with("INST(") {
            continue;
        }
        if let Some(p) = parse_inst_line(line) {
            patterns.push(p);
        }
    }

    // Sort by specificity (most specific first) for correct matching.
    // `sort_by_key` is stable in Rust, matching upstream's `std::stable_sort`.
    patterns.sort_by_key(|p| std::cmp::Reverse(p.specificity));

    // Decoder exceptions — these display names must come first regardless
    // of specificity. Mirrors upstream `decoder/a64.h:48-57` which uses
    // `std::stable_partition` to hoist them. Without this, SHL_2 / USHR_2
    // / SSHR_2 (specificity 14) match before MOVI (specificity 13) when
    // immh==0; the shift handlers then call `DecodeError()` which is
    // `unreachable!()` and the JIT panics.
    let comes_first: std::collections::HashSet<&str> = [
        "MOVI, MVNI, ORR, BIC (vector, immediate)",
        "FMOV (vector, immediate)",
        "Unallocated SIMD modified immediate",
    ]
    .iter()
    .copied()
    .collect();
    let mut early: Vec<Pattern> = Vec::new();
    let mut rest: Vec<Pattern> = Vec::new();
    for p in patterns.drain(..) {
        if comes_first.contains(p.display_name.as_str()) {
            early.push(p);
        } else {
            rest.push(p);
        }
    }
    early.append(&mut rest);
    patterns = early;

    // Generate the visitor trait and decode function
    let dest_path = Path::new(&out_dir).join("a64_decoder_gen.rs");
    let mut f = fs::File::create(&dest_path).expect("Failed to create decoder_gen.rs");

    // Collect unique instruction names for the visitor trait
    let mut unique_names: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for p in &patterns {
        if !seen.contains(&p.name) {
            seen.insert(p.name.clone());
            unique_names.push(p.name.clone());
        }
    }

    // Generate the InstructionName enum
    writeln!(f, "/// All A64 instruction names from a64.inc.").unwrap();
    writeln!(f, "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]").unwrap();
    writeln!(f, "#[allow(non_camel_case_types)]").unwrap();
    writeln!(f, "pub enum A64InstructionName {{").unwrap();
    for name in &unique_names {
        writeln!(f, "    {},", name).unwrap();
    }
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    // Generate decode function using 2-tier lookup
    // Tier 1: fast index = ((insn >> 10) & 0x00F) | ((insn >> 18) & 0xFF0)
    // Tier 2: linear scan within subtable

    // Build the fast lookup table
    let mut table: HashMap<u16, Vec<usize>> = HashMap::new();
    for (idx, p) in patterns.iter().enumerate() {
        // Generate all possible fast-index values that match this pattern
        let fast_mask = fast_lookup_mask(&p.bitstring);
        let fast_expect = fast_lookup_expect(&p.bitstring);

        // Iterate over all 4096 possible fast indices
        for fi in 0u16..4096 {
            if fi & fast_mask == fast_expect {
                table.entry(fi).or_default().push(idx);
            }
        }
    }

    // Generate the decode function
    writeln!(f, "/// Decoded instruction with name and raw encoding.").unwrap();
    writeln!(f, "#[derive(Debug, Clone, Copy)]").unwrap();
    writeln!(f, "pub struct DecodedInst {{").unwrap();
    writeln!(f, "    pub name: A64InstructionName,").unwrap();
    writeln!(f, "    pub raw: u32,").unwrap();
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    // Generate static pattern data
    writeln!(f, "struct PatternEntry {{").unwrap();
    writeln!(f, "    mask: u32,").unwrap();
    writeln!(f, "    expect: u32,").unwrap();
    writeln!(f, "    name: A64InstructionName,").unwrap();
    writeln!(f, "}}").unwrap();
    writeln!(f).unwrap();

    writeln!(f, "static PATTERNS: &[PatternEntry] = &[").unwrap();
    for p in &patterns {
        writeln!(f, "    PatternEntry {{ mask: {:#010x}, expect: {:#010x}, name: A64InstructionName::{} }},",
            p.mask, p.expect, p.name).unwrap();
    }
    writeln!(f, "];").unwrap();
    writeln!(f).unwrap();

    // Generate the fast lookup table as a static array
    // Each entry is a range into a subtable array
    let mut subtable: Vec<u16> = Vec::new(); // indices into PATTERNS
    let mut table_entries: Vec<(u32, u32)> = Vec::new(); // (start, len) for each fast index

    for fi in 0u16..4096 {
        if let Some(indices) = table.get(&fi) {
            let start = subtable.len() as u32;
            for &idx in indices {
                subtable.push(idx as u16);
            }
            table_entries.push((start, indices.len() as u32));
        } else {
            table_entries.push((0, 0));
        }
    }

    writeln!(f, "static SUBTABLE: &[u16] = &[").unwrap();
    for chunk in subtable.chunks(16) {
        write!(f, "    ").unwrap();
        for (j, &val) in chunk.iter().enumerate() {
            if j > 0 {
                write!(f, ", ").unwrap();
            }
            write!(f, "{}", val).unwrap();
        }
        writeln!(f, ",").unwrap();
    }
    writeln!(f, "];").unwrap();
    writeln!(f).unwrap();

    writeln!(f, "static FAST_TABLE: &[(u32, u32)] = &[").unwrap();
    for chunk in table_entries.chunks(8) {
        write!(f, "    ").unwrap();
        for (j, &(s, l)) in chunk.iter().enumerate() {
            if j > 0 {
                write!(f, ", ").unwrap();
            }
            write!(f, "({}, {})", s, l).unwrap();
        }
        writeln!(f, ",").unwrap();
    }
    writeln!(f, "];").unwrap();
    writeln!(f).unwrap();

    // Generate the decode function
    writeln!(f, "/// Decode a 32-bit ARM64 instruction.").unwrap();
    writeln!(f, "/// Returns None for unrecognized encodings.").unwrap();
    writeln!(f, "pub fn decode(insn: u32) -> Option<DecodedInst> {{").unwrap();
    writeln!(
        f,
        "    let fast_idx = (((insn >> 10) & 0x00F) | ((insn >> 18) & 0xFF0)) as usize;"
    )
    .unwrap();
    writeln!(f, "    let (start, len) = FAST_TABLE[fast_idx];").unwrap();
    writeln!(f, "    for i in start..(start + len) {{").unwrap();
    writeln!(f, "        let pat_idx = SUBTABLE[i as usize] as usize;").unwrap();
    writeln!(f, "        let pat = &PATTERNS[pat_idx];").unwrap();
    writeln!(f, "        if insn & pat.mask == pat.expect {{").unwrap();
    writeln!(
        f,
        "            return Some(DecodedInst {{ name: pat.name, raw: insn }});"
    )
    .unwrap();
    writeln!(f, "        }}").unwrap();
    writeln!(f, "    }}").unwrap();
    writeln!(f, "    None").unwrap();
    writeln!(f, "}}").unwrap();

    eprintln!(
        "Generated decoder with {} patterns, {} unique instructions, subtable size {}",
        patterns.len(),
        unique_names.len(),
        subtable.len()
    );
}

fn parse_inst_line(line: &str) -> Option<Pattern> {
    // INST(NAME, "display", "bitstring")
    let line = line.strip_prefix("INST(")?;
    let closing_parenthesis = line.rfind(')')?;
    let line = &line[..=closing_parenthesis];
    let line = line.strip_suffix(')')?;

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in line.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
        } else if ch == ',' && !in_quotes {
            parts.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(ch);
        }
    }
    parts.push(current.trim().to_string());

    if parts.len() < 3 {
        return None;
    }

    let name = parts[0].trim().to_string();
    let display_name = parts[1].trim().to_string();
    let bitstring = parts[2].trim().to_string();

    if bitstring.len() != 32 {
        eprintln!(
            "Warning: bitstring for {} is {} chars: '{}'",
            name,
            bitstring.len(),
            bitstring
        );
        return None;
    }

    let mut mask = 0u32;
    let mut expect = 0u32;
    let mut specificity = 0u32;

    for (i, ch) in bitstring.chars().enumerate() {
        let bit_pos = 31 - i;
        match ch {
            '0' => {
                mask |= 1 << bit_pos;
                specificity += 1;
            }
            '1' => {
                mask |= 1 << bit_pos;
                expect |= 1 << bit_pos;
                specificity += 1;
            }
            // Variable fields: lowercase letters and uppercase letters
            _ => {
                // Don't care bit - not part of mask
            }
        }
    }

    Some(Pattern {
        name,
        display_name,
        bitstring,
        mask,
        expect,
        specificity,
    })
}

/// Extract the 12-bit fast lookup mask from a bitstring.
/// Fast index = ((insn >> 10) & 0x00F) | ((insn >> 18) & 0xFF0)
/// That extracts bits [13:10] and bits [29:22].
fn fast_lookup_mask(bitstring: &str) -> u16 {
    let mut mask = 0u16;
    // Bits [29:22] -> fast index bits [11:4]
    for bit in 22..=29 {
        let char_idx = 31 - bit;
        let ch = bitstring.as_bytes()[char_idx as usize] as char;
        if ch == '0' || ch == '1' {
            mask |= 1 << (bit - 22 + 4);
        }
    }
    // Bits [13:10] -> fast index bits [3:0]
    for bit in 10..=13 {
        let char_idx = 31 - bit;
        let ch = bitstring.as_bytes()[char_idx as usize] as char;
        if ch == '0' || ch == '1' {
            mask |= 1 << (bit - 10);
        }
    }
    mask
}

/// Extract the 12-bit fast lookup expected value from a bitstring.
fn fast_lookup_expect(bitstring: &str) -> u16 {
    let mut expect = 0u16;
    // Bits [29:22] -> fast index bits [11:4]
    for bit in 22..=29 {
        let char_idx = 31 - bit;
        let ch = bitstring.as_bytes()[char_idx as usize] as char;
        if ch == '1' {
            expect |= 1 << (bit - 22 + 4);
        }
    }
    // Bits [13:10] -> fast index bits [3:0]
    for bit in 10..=13 {
        let char_idx = 31 - bit;
        let ch = bitstring.as_bytes()[char_idx as usize] as char;
        if ch == '1' {
            expect |= 1 << (bit - 10);
        }
    }
    expect
}

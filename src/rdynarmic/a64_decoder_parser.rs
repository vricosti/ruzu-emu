/// Represents one active `INST(...)` pattern from Eden's `a64.inc` table.
pub(crate) struct Pattern {
    pub(crate) name: String,
    pub(crate) display_name: String,
    pub(crate) bitstring: String,
    pub(crate) mask: u32,
    pub(crate) expect: u32,
    pub(crate) specificity: u32,
}

fn closing_parenthesis(line: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut escaped = false;

    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_quotes && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_quotes = !in_quotes;
        } else if ch == ')' && !in_quotes {
            return Some(index);
        }
    }

    None
}

pub(crate) fn parse_inst_line(line: &str) -> Option<Pattern> {
    // INST(NAME, "display", "bitstring")
    let line = line.strip_prefix("INST(")?;
    let closing_parenthesis = closing_parenthesis(line)?;
    let line = &line[..closing_parenthesis];

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;

    for ch in line.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if in_quotes && ch == '\\' {
            current.push(ch);
            escaped = true;
        } else if ch == '"' {
            in_quotes = !in_quotes;
        } else if ch == ',' && !in_quotes {
            parts.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(ch);
        }
    }
    parts.push(current.trim().to_string());

    if parts.len() != 3 {
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
            _ => {}
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

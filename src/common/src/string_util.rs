//! Port of zuyu/src/common/string_util.h and zuyu/src/common/string_util.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05

/// Make a string lowercase.
pub fn to_lower(s: &str) -> String {
    s.to_lowercase()
}

/// Make a string uppercase.
pub fn to_upper(s: &str) -> String {
    s.to_uppercase()
}

/// Create a string from a byte buffer, stopping at the first NUL byte.
pub fn string_from_buffer(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).into_owned()
}

/// Strip leading and trailing whitespace (spaces, tabs, carriage returns, newlines).
pub fn strip_spaces(s: &str) -> String {
    s.trim_matches(&[' ', '\t', '\r', '\n'][..]).to_string()
}

/// Strip surrounding double quotes from a string.
/// Assumes the string has already been space-stripped.
pub fn strip_quotes(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Convert a bool to "True" or "False".
pub fn string_from_bool(value: bool) -> &'static str {
    if value {
        "True"
    } else {
        "False"
    }
}

/// Replace all tab characters with `tab_size` spaces.
pub fn tabs_to_spaces(tab_size: usize, input: &str) -> String {
    input.replace('\t', &" ".repeat(tab_size))
}

/// Split a string by a delimiter character.
pub fn split_string(s: &str, delim: char) -> Vec<String> {
    s.split(delim).map(|part| part.to_string()).collect()
}

/// Split a full file path into (directory, filename_without_extension, extension).
/// Returns `None` if the path is empty.
pub fn split_path(full_path: &str) -> Option<(String, String, String)> {
    if full_path.is_empty() {
        return None;
    }

    let dir_end = full_path
        .rfind(|c| c == '/' || (cfg!(windows) && matches!(c, '\\' | ':')))
        .map(|i| i + 1).unwrap_or(0);

    let fname_end = full_path
        .rfind('.')
        .and_then(|i| if i < dir_end { None } else { Some(i) })
        .unwrap_or(full_path.len());

    let path = full_path[..dir_end].to_string();
    let filename = full_path[dir_end..fname_end].to_string();
    let extension = full_path[fname_end..].to_string();

    Some((path, filename, extension))
}

/// Replace all occurrences of `src` with `dest` in `result`.
pub fn replace_all(result: &str, src: &str, dest: &str) -> String {
    if src == dest {
        return result.to_string();
    }
    result.replace(src, dest)
}

/// Convert a UTF-16 slice to a UTF-8 String.
pub fn utf16_to_utf8(input: &[u16]) -> String {
    String::from_utf16_lossy(input)
}

/// Convert a UTF-8 string to a Vec of UTF-16 code units.
pub fn utf8_to_utf16(input: &str) -> Vec<u16> {
    input.encode_utf16().collect()
}

/// Convert a UTF-8 string to a Vec of UTF-32 code points (char values).
pub fn utf8_to_utf32(input: &str) -> Vec<u32> {
    input.chars().map(|c| c as u32).collect()
}

/// Create a u16 string from a raw u16 buffer with a given length.
pub fn u16_string_from_buffer(input: &[u16], length: usize) -> Vec<u16> {
    let len = length.min(input.len());
    input[..len].to_vec()
}

/// Compares the slice `data` to the null-terminated string `other` for equality.
pub fn compare_partial_string(data: &[u8], other: &str) -> bool {
    let other_bytes = other.as_bytes();
    if data.len() != other_bytes.len() {
        return false;
    }
    data == other_bytes
}

/// Creates a String from a fixed-size NUL-terminated buffer.
/// If the buffer isn't NUL-terminated then the string ends at max_len characters.
pub fn string_from_fixed_zero_terminated_buffer(buffer: &[u8], max_len: usize) -> String {
    let limit = buffer.len().min(max_len);
    let end = buffer[..limit]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(limit);
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// Creates a UTF-16 Vec from a fixed-size NUL-terminated u16 buffer.
pub fn utf16_string_from_fixed_zero_terminated_buffer(buffer: &[u16], max_len: usize) -> Vec<u16> {
    let limit = buffer.len().min(max_len);
    let end = buffer[..limit]
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(limit);
    buffer[..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_lower() {
        assert_eq!(to_lower("Hello World"), "hello world");
    }

    #[test]
    fn test_to_upper() {
        assert_eq!(to_upper("Hello World"), "HELLO WORLD");
    }

    #[test]
    fn test_string_from_buffer() {
        assert_eq!(string_from_buffer(b"hello\0world"), "hello");
        assert_eq!(string_from_buffer(b"hello"), "hello");
    }

    #[test]
    fn test_strip_spaces() {
        assert_eq!(strip_spaces("  hej  "), "hej");
        assert_eq!(strip_spaces("\t hello \n"), "hello");
    }

    #[test]
    fn test_strip_quotes() {
        assert_eq!(strip_quotes("\"hello\""), "hello");
        assert_eq!(strip_quotes("hello"), "hello");
    }

    #[test]
    fn test_split_path() {
        let (path, name, ext) = split_path("/home/user/file.txt").unwrap();
        assert_eq!(path, "/home/user/");
        assert_eq!(name, "file");
        assert_eq!(ext, ".txt");
    }

    #[test]
    fn split_path_respects_host_directory_separators() {
        let (directory, filename, extension) = split_path(r"C:\homebrew\demo.nro").unwrap();
        if cfg!(windows) {
            assert_eq!(directory, "C:\\homebrew\\");
            assert_eq!(filename, "demo");
        } else {
            assert_eq!(directory, "");
            assert_eq!(filename, r"C:\homebrew\demo");
        }
        assert_eq!(extension, ".nro");
    }

    #[test]
    fn test_replace_all() {
        assert_eq!(replace_all("aabaa", "a", "b"), "bbbbb");
    }

    #[test]
    fn test_string_from_fixed_zero_terminated() {
        assert_eq!(
            string_from_fixed_zero_terminated_buffer(b"hello\0extra", 11),
            "hello"
        );
        assert_eq!(string_from_fixed_zero_terminated_buffer(b"hello", 3), "hel");
    }
}

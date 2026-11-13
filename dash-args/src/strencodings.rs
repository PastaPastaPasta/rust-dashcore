//! The string helpers ArgsManager depends on (Core: `util/strencodings.*`, `util/string.h`).

use crate::univalue::{FromChars, from_chars_i64};

/// Core's default `TrimString` pattern.
pub const DEFAULT_TRIM_PATTERN: &str = " \x0c\n\r\t\x0b";

/// Core: `TrimStringView(str, pattern)`.
pub fn trim_string<'a>(s: &'a str, pattern: &str) -> &'a str {
    s.trim_matches(|c| pattern.contains(c))
}

/// Core: `LocaleIndependentAtoi<int64_t>`: `atoi` under the "C" locale, saturating on overflow.
pub fn locale_independent_atoi(s: &str) -> i64 {
    let mut s = trim_string(s, DEFAULT_TRIM_PATTERN);
    if let Some(rest) = s.strip_prefix('+') {
        if rest.starts_with('-') {
            return 0;
        }
        s = rest;
    }
    match from_chars_i64(s.as_bytes()).0 {
        FromChars::Ok(value) => value,
        FromChars::OutOfRange if s.starts_with('-') => i64::MIN,
        FromChars::OutOfRange => i64::MAX,
        FromChars::Invalid => 0,
    }
}

/// Core: `LocaleIndependentAtoi<int>`, as used by `InterpretBool`.
fn locale_independent_atoi_int(s: &str) -> i32 {
    locale_independent_atoi(s).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Core: `InterpretBool` (util/system.cpp): an empty string is true, anything else is true
/// when `atoi` of it is non-zero, so `-foo=true` is false.
pub fn interpret_bool(value: &str) -> bool {
    value.is_empty() || locale_independent_atoi_int(value) != 0
}

/// Core: `FormatParagraph(in, width, indent)`: word-wraps at spaces, indenting continuation
/// lines by `indent` columns. Embedded newlines are kept and start an unindented line.
pub fn format_paragraph(input: &str, width: usize, indent: usize) -> String {
    assert!(width >= indent);
    let bytes = input.as_bytes();
    let mut out = String::new();
    let mut ptr = 0;
    let mut indented = 0;
    while ptr < bytes.len() {
        let lineend = find_first_of(bytes, b"\n", ptr).unwrap_or(bytes.len());
        let linelen = lineend - ptr;
        let rem_width = width - indented;
        if linelen <= rem_width {
            out.push_str(&input[ptr..(lineend + 1).min(bytes.len())]);
            ptr = lineend + 1;
            indented = 0;
        } else {
            let mut finalspace = find_last_of(bytes, b" \n", ptr + rem_width);
            if finalspace.is_none_or(|f| f < ptr) {
                // No place to break; just include the entire word and move on.
                finalspace = find_first_of(bytes, b"\n ", ptr);
                if finalspace.is_none() {
                    out.push_str(&input[ptr..]);
                    break;
                }
            }
            let finalspace = finalspace.expect("set above");
            out.push_str(&input[ptr..finalspace]);
            out.push('\n');
            if bytes[finalspace] == b'\n' {
                indented = 0;
            } else if indent > 0 {
                out.extend(std::iter::repeat_n(' ', indent));
                indented = indent;
            }
            ptr = finalspace + 1;
        }
    }
    out
}

/// `std::string_view::find_first_of(set, pos)`.
fn find_first_of(s: &[u8], set: &[u8], pos: usize) -> Option<usize> {
    (pos..s.len()).find(|&i| set.contains(&s[i]))
}

/// `std::string_view::find_last_of(set, pos)`: the last match at or before `pos`.
fn find_last_of(s: &[u8], set: &[u8], pos: usize) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    (0..=pos.min(s.len() - 1)).rev().find(|&i| set.contains(&s[i]))
}

/// Core: `MakeUnorderedList`.
pub fn make_unordered_list(items: &[String]) -> String {
    items.iter().map(|item| format!("- {item}")).collect::<Vec<_>>().join("\n")
}

/// `std::quoted(s, '"', '&')`, Core's `fs::quoted`.
pub fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '&' {
            out.push('&');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atoi_matches_core() {
        // Core: src/test/util_tests.cpp test_LocaleIndependentAtoi; its int32_t cases behave the
        // same at int64_t width except for the saturation points, which use the int64_t cases.
        assert_eq!(locale_independent_atoi("1234"), 1_234);
        assert_eq!(locale_independent_atoi("0"), 0);
        assert_eq!(locale_independent_atoi("01234"), 1_234);
        assert_eq!(locale_independent_atoi("-1234"), -1_234);
        assert_eq!(locale_independent_atoi(" 1"), 1);
        assert_eq!(locale_independent_atoi("1 "), 1);
        assert_eq!(locale_independent_atoi("1a"), 1);
        assert_eq!(locale_independent_atoi("1.1"), 1);
        assert_eq!(locale_independent_atoi("1.9"), 1);
        assert_eq!(locale_independent_atoi("+01.9"), 1);
        assert_eq!(locale_independent_atoi("-1"), -1);
        assert_eq!(locale_independent_atoi(" -1"), -1);
        assert_eq!(locale_independent_atoi("-1 "), -1);
        assert_eq!(locale_independent_atoi(" -1 "), -1);
        assert_eq!(locale_independent_atoi("+1"), 1);
        assert_eq!(locale_independent_atoi(" +1"), 1);
        assert_eq!(locale_independent_atoi(" +1 "), 1);
        assert_eq!(locale_independent_atoi("+-1"), 0);
        assert_eq!(locale_independent_atoi("-+1"), 0);
        assert_eq!(locale_independent_atoi("++1"), 0);
        assert_eq!(locale_independent_atoi("--1"), 0);
        assert_eq!(locale_independent_atoi(""), 0);
        assert_eq!(locale_independent_atoi("aap"), 0);
        assert_eq!(locale_independent_atoi("0x1"), 0);
        assert_eq!(locale_independent_atoi("-32482348723847471234"), i64::MIN);
        assert_eq!(locale_independent_atoi("32482348723847471234"), i64::MAX);
        assert_eq!(locale_independent_atoi("-9223372036854775809"), i64::MIN);
        assert_eq!(locale_independent_atoi("-9223372036854775808"), i64::MIN);
        assert_eq!(locale_independent_atoi("9223372036854775807"), i64::MAX);
        assert_eq!(locale_independent_atoi("9223372036854775808"), i64::MAX);
        assert_eq!(locale_independent_atoi("+-"), 0);
        assert_eq!(locale_independent_atoi("ox1"), 0);
    }

    #[test]
    fn format_paragraph_matches_core() {
        // Core: src/test/util_tests.cpp test_FormatParagraph.
        assert_eq!(format_paragraph("", 79, 0), "");
        assert_eq!(format_paragraph("test", 79, 0), "test");
        assert_eq!(format_paragraph(" test", 79, 0), " test");
        assert_eq!(format_paragraph("test test", 79, 0), "test test");
        assert_eq!(format_paragraph("test test", 4, 0), "test\ntest");
        assert_eq!(format_paragraph("testerde test", 4, 0), "testerde\ntest");
        assert_eq!(format_paragraph("test test", 4, 4), "test\n    test");
        // Make sure we don't indent a fully-new line following a too-long line ending.
        assert_eq!(format_paragraph("test test\nabc", 4, 4), "test\n    test\nabc");
        assert_eq!(
            format_paragraph(
                "This_is_a_very_long_test_string_without_any_spaces_so_it_should_just_get_returned_as_is_despite_the_length until it gets here",
                79,
                0
            ),
            "This_is_a_very_long_test_string_without_any_spaces_so_it_should_just_get_returned_as_is_despite_the_length\nuntil it gets here"
        );
        assert_eq!(
            format_paragraph(
                "a b c d e f g h i j k l m n o p q r s t u v w x y z 1 2 3 4 5 6 7 8 9 a b c de f g h i j k l m n o p",
                79,
                0
            ),
            "a b c d e f g h i j k l m n o p q r s t u v w x y z 1 2 3 4 5 6 7 8 9 a b c de\nf g h i j k l m n o p"
        );
        assert_eq!(
            format_paragraph(
                "x\na b c d e f g h i j k l m n o p q r s t u v w x y z 1 2 3 4 5 6 7 8 9 a b c de f g h i j k l m n o p",
                79,
                0
            ),
            "x\na b c d e f g h i j k l m n o p q r s t u v w x y z 1 2 3 4 5 6 7 8 9 a b c de\nf g h i j k l m n o p"
        );
        // Indent should be included in length of lines.
        assert_eq!(
            format_paragraph(
                "x\na b c d e f g h i j k l m n o p q r s t u v w x y z 1 2 3 4 5 6 7 8 9 a b c de f g h i j k l m n o p q r s t u v w x y z 0 1 2 3 4 5 6 7 8 9 a b c d e fg h i j k",
                79,
                4
            ),
            "x\na b c d e f g h i j k l m n o p q r s t u v w x y z 1 2 3 4 5 6 7 8 9 a b c de\n    f g h i j k l m n o p q r s t u v w x y z 0 1 2 3 4 5 6 7 8 9 a b c d e fg\n    h i j k"
        );
        assert_eq!(
            format_paragraph(
                "This is a very long test string. This is a second sentence in the very long test string.",
                79,
                0
            ),
            "This is a very long test string. This is a second sentence in the very long\ntest string."
        );
        assert_eq!(
            format_paragraph(
                "This is a very long test string.\nThis is a second sentence in the very long test string. This is a third sentence in the very long test string.",
                79,
                0
            ),
            "This is a very long test string.\nThis is a second sentence in the very long test string. This is a third\nsentence in the very long test string."
        );
        assert_eq!(
            format_paragraph(
                "This is a very long test string.\n\nThis is a second sentence in the very long test string. This is a third sentence in the very long test string.",
                79,
                0
            ),
            "This is a very long test string.\n\nThis is a second sentence in the very long test string. This is a third\nsentence in the very long test string."
        );
        assert_eq!(
            format_paragraph(
                "Testing that normal newlines do not get indented.\nLike here.",
                79,
                0
            ),
            "Testing that normal newlines do not get indented.\nLike here."
        );
    }

    #[test]
    fn quoted_uses_ampersand_escape() {
        assert_eq!(quoted("a\"b&c\\d"), "\"a&\"b&&c\\d\"");
    }
}

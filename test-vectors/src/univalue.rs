//! A small JSON value type that behaves like Core's `UniValue` where the vector harness depends
//! on it.
//!
//! - Numbers keep their source text (`UniValue::VNUM` stores a string), so `write()` reproduces
//!   them byte for byte. The governance proposal harness hex-encodes `objProposal.write()`.
//! - Objects keep key order and allow duplicate keys; [`UniValue::find_value`] returns the first.
//! - [`UniValue::write`] is the compact form with Core's string escaping.
//!
//! `serde_json` is avoided on purpose: its number handling and key order would change those
//! bytes, and enabling the features that fix that would affect every crate in the workspace.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum UniValue {
    Null,
    Bool(bool),
    /// The number exactly as written in the source.
    Num(String),
    Str(String),
    Arr(Vec<UniValue>),
    Obj(Vec<(String, UniValue)>),
}

/// A JSON parse error with the byte offset it was detected at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub offset: usize,
    pub message: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON error at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ParseError {}

/// The `UniValue` accessor that was used on a value of the wrong type or range.
///
/// Core throws `std::runtime_error` in these cases, which its harness reports as a bad test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError(pub &'static str);

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for TypeError {}

/// Matches `MAX_JSON_DEPTH` in `univalue_read.cpp`.
const MAX_DEPTH: usize = 512;

impl UniValue {
    pub fn parse(text: &str) -> Result<UniValue, ParseError> {
        let mut p = Parser {
            text,
            s: text.as_bytes(),
            i: 0,
        };
        p.skip_ws();
        let v = p.value(0)?;
        p.skip_ws();
        if p.i != p.s.len() {
            return Err(p.err("trailing characters"));
        }
        Ok(v)
    }

    pub fn len(&self) -> usize {
        match self {
            UniValue::Arr(a) => a.len(),
            UniValue::Obj(o) => o.len(),
            _ => 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_null(&self) -> bool {
        matches!(self, UniValue::Null)
    }

    /// `get_str()`
    pub fn get_str(&self) -> Result<&str, TypeError> {
        match self {
            UniValue::Str(s) => Ok(s),
            _ => Err(TypeError("JSON value is not a string as expected")),
        }
    }

    /// `get_bool()`
    pub fn get_bool(&self) -> Result<bool, TypeError> {
        match self {
            UniValue::Bool(b) => Ok(*b),
            _ => Err(TypeError("JSON value is not a boolean as expected")),
        }
    }

    /// `get_array()`
    pub fn get_array(&self) -> Result<&[UniValue], TypeError> {
        match self {
            UniValue::Arr(a) => Ok(a),
            _ => Err(TypeError("JSON value is not an array as expected")),
        }
    }

    /// `get_obj()`
    pub fn get_obj(&self) -> Result<&[(String, UniValue)], TypeError> {
        match self {
            UniValue::Obj(o) => Ok(o),
            _ => Err(TypeError("JSON value is not an object as expected")),
        }
    }

    /// `getInt<int>()`: the whole number text must parse as an `i32` (`std::from_chars`).
    pub fn get_i32(&self) -> Result<i32, TypeError> {
        match self {
            UniValue::Num(n) => {
                from_chars_integer::<i32>(n).ok_or(TypeError("JSON integer out of range"))
            }
            _ => Err(TypeError("JSON value is not a number as expected")),
        }
    }

    /// `getInt<int64_t>()`
    pub fn get_i64(&self) -> Result<i64, TypeError> {
        match self {
            UniValue::Num(n) => {
                from_chars_integer::<i64>(n).ok_or(TypeError("JSON integer out of range"))
            }
            _ => Err(TypeError("JSON value is not a number as expected")),
        }
    }

    /// `find_value(key)`: the first member with that key, or `Null` (like Core's static null).
    pub fn find_value(&self, key: &str) -> &UniValue {
        static NULL: UniValue = UniValue::Null;
        if let UniValue::Obj(o) = self {
            for (k, v) in o {
                if k == key {
                    return v;
                }
            }
        }
        &NULL
    }

    /// `operator[](size_t)`: the array element, or `Null` out of range or on a non-array.
    pub fn at(&self, i: usize) -> &UniValue {
        static NULL: UniValue = UniValue::Null;
        match self {
            UniValue::Arr(a) => a.get(i).unwrap_or(&NULL),
            UniValue::Obj(o) => o.get(i).map(|(_, v)| v).unwrap_or(&NULL),
            _ => &NULL,
        }
    }

    /// `write()` with no pretty-printing.
    pub fn write(&self) -> String {
        let mut s = String::new();
        self.write_into(&mut s);
        s
    }

    fn write_into(&self, s: &mut String) {
        match self {
            UniValue::Null => s.push_str("null"),
            UniValue::Bool(b) => s.push_str(if *b {
                "true"
            } else {
                "false"
            }),
            UniValue::Num(n) => s.push_str(n),
            UniValue::Str(t) => {
                s.push('"');
                json_escape(t, s);
                s.push('"');
            }
            UniValue::Arr(a) => {
                s.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i != 0 {
                        s.push(',');
                    }
                    v.write_into(s);
                }
                s.push(']');
            }
            UniValue::Obj(o) => {
                s.push('{');
                for (i, (k, v)) in o.iter().enumerate() {
                    if i != 0 {
                        s.push(',');
                    }
                    s.push('"');
                    json_escape(k, s);
                    s.push_str("\":");
                    v.write_into(s);
                }
                s.push('}');
            }
        }
    }
}

/// `std::from_chars` for integers: optional leading `-` (no `+`), at least one digit, nothing
/// else, and the value must fit.
fn from_chars_integer<T: std::str::FromStr>(s: &str) -> Option<T> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `univalue_escapes.h`: control characters, `"`, `\` and DEL are escaped; everything else
/// (including `/` and non-ASCII) is written as is.
fn json_escape(input: &str, out: &mut String) {
    for ch in input.chars() {
        match ch {
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

struct Parser<'a> {
    text: &'a str,
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn err(&self, message: &'static str) -> ParseError {
        ParseError {
            offset: self.i,
            message,
        }
    }

    fn skip_ws(&mut self) {
        // `json_isspace`
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn value(&mut self, depth: usize) -> Result<UniValue, ParseError> {
        match self.peek() {
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => Ok(UniValue::Str(self.string()?)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b't') => self.keyword("true", UniValue::Bool(true)),
            Some(b'f') => self.keyword("false", UniValue::Bool(false)),
            Some(b'n') => self.keyword("null", UniValue::Null),
            _ => Err(self.err("unexpected token")),
        }
    }

    fn keyword(&mut self, word: &'static str, v: UniValue) -> Result<UniValue, ParseError> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.err("bad keyword"))
        }
    }

    fn object(&mut self, depth: usize) -> Result<UniValue, ParseError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        self.i += 1;
        let mut members = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(UniValue::Obj(members));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("expected object key"));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.err("expected ':'"));
            }
            self.i += 1;
            self.skip_ws();
            let v = self.value(depth)?;
            members.push((key, v));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(UniValue::Obj(members));
                }
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<UniValue, ParseError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        self.i += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(UniValue::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(UniValue::Arr(items));
                }
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
    }

    fn number(&mut self) -> Result<UniValue, ParseError> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        // No leading zeros, and a digit must follow the sign (`getJsonToken`).
        let int_start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        let int_len = self.i - int_start;
        if int_len == 0 || (int_len > 1 && self.s[int_start] == b'0') {
            return Err(self.err("bad number"));
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            let frac = self.i;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == frac {
                return Err(self.err("bad fraction"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let exp = self.i;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == exp {
                return Err(self.err("bad exponent"));
            }
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).expect("ASCII digits");
        Ok(UniValue::Num(text.to_owned()))
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1; // opening quote
        let mut out = String::new();
        let mut high_surrogate: Option<u32> = None;
        loop {
            let Some(b) = self.peek() else {
                return Err(self.err("unterminated string"));
            };
            if b < 0x20 {
                return Err(self.err("control character in string"));
            }
            if b == b'"' {
                self.i += 1;
                if high_surrogate.is_some() {
                    return Err(self.err("lone surrogate"));
                }
                return Ok(out);
            }
            if b == b'\\' {
                self.i += 1;
                let Some(e) = self.peek() else {
                    return Err(self.err("unterminated escape"));
                };
                let simple = match e {
                    b'"' => Some('"'),
                    b'\\' => Some('\\'),
                    b'/' => Some('/'),
                    b'b' => Some('\u{08}'),
                    b'f' => Some('\u{0c}'),
                    b'n' => Some('\n'),
                    b'r' => Some('\r'),
                    b't' => Some('\t'),
                    b'u' => None,
                    _ => return Err(self.err("bad escape")),
                };
                if let Some(c) = simple {
                    if high_surrogate.is_some() {
                        return Err(self.err("lone surrogate"));
                    }
                    out.push(c);
                    self.i += 1;
                    continue;
                }
                let hex = self.s.get(self.i + 1..self.i + 5).ok_or(self.err("short \\u escape"))?;
                let hex = std::str::from_utf8(hex).map_err(|_| self.err("bad \\u escape"))?;
                if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(self.err("bad \\u escape"));
                }
                let cp = u32::from_str_radix(hex, 16).map_err(|_| self.err("bad \\u escape"))?;
                self.i += 5;
                match (high_surrogate.take(), cp) {
                    (Some(hi), 0xdc00..=0xdfff) => {
                        let c = 0x10000 + ((hi - 0xd800) << 10) + (cp - 0xdc00);
                        out.push(char::from_u32(c).ok_or(self.err("bad surrogate pair"))?);
                    }
                    (Some(_), _) => return Err(self.err("lone surrogate")),
                    (None, 0xd800..=0xdbff) => high_surrogate = Some(cp),
                    (None, 0xdc00..=0xdfff) => return Err(self.err("lone surrogate")),
                    (None, c) => out.push(char::from_u32(c).ok_or(self.err("bad code point"))?),
                }
                continue;
            }
            if high_surrogate.is_some() {
                return Err(self.err("lone surrogate"));
            }
            // Copy one UTF-8 scalar. `self.i` is always on a char boundary: it only advances over
            // ASCII bytes and whole scalars.
            let c = self.text[self.i..].chars().next().expect("non-empty");
            out.push(c);
            self.i += c.len_utf8();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_key_order_survive_a_round_trip() {
        let src = r#"{"b":25.12345678,"a":[1,-0,1e5,"x\/y\u007f\n"],"c":null,"d":true}"#;
        let v = UniValue::parse(src).unwrap();
        // `/` is not escaped on output, DEL is.
        assert_eq!(
            v.write(),
            r#"{"b":25.12345678,"a":[1,-0,1e5,"x/y\u007f\n"],"c":null,"d":true}"#
        );
        assert_eq!(v.find_value("a").at(2), &UniValue::Num("1e5".into()));
        assert!(v.find_value("missing").is_null());
    }

    #[test]
    fn get_int_follows_from_chars() {
        assert_eq!(UniValue::Num("-1".into()).get_i32(), Ok(-1));
        assert!(UniValue::Num("1.0".into()).get_i32().is_err());
        assert!(UniValue::Num("2147483648".into()).get_i32().is_err());
        assert!(UniValue::Num("+1".into()).get_i32().is_err());
        assert!(UniValue::Str("1".into()).get_i32().is_err());
    }

    #[test]
    fn rejects_what_the_core_reader_rejects() {
        for bad in ["[1,]", "[01]", "[-]", "[1.]", "{\"a\" 1}", "[1] x", "\"\\q\"", "\"\u{1}\"", ""]
        {
            assert!(UniValue::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(UniValue::parse("\"\\ud83d\\ude00\"").unwrap(), UniValue::Str("😀".into()));
        assert!(UniValue::parse("\"\\ud83d\"").is_err());
    }
}

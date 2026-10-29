//! A lenient JSON reader for dashd's RPC replies that keeps every number as Core's text.
//!
//! `serde_json::Value` turns `25000.00000000` into an `f64`; the golden file needs the text
//! (spec §3, §4.6, §9.4). This reader accepts standard JSON and produces the
//! [`dash_statedigest::Value`] payload form: every number is a [`Value::Num`] holding the digits
//! as printed, strings are byte strings with escapes decoded, and object members are sorted
//! bytewise (the order an `S` payload requires). Duplicate keys are refused.

use std::fmt;

use dash_statedigest::{Value, is_number_text};

/// The deepest nesting the reader follows.
const MAX_DEPTH: usize = 160;

/// Why a reply could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    /// Byte offset into the input.
    pub offset: usize,
    pub why: &'static str,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid JSON at byte {}: {}", self.offset, self.why)
    }
}

impl std::error::Error for JsonError {}

/// Reads one JSON document (surrounding whitespace allowed) into a payload [`Value`].
pub fn parse(input: &[u8]) -> Result<Value, JsonError> {
    let mut p = Reader {
        buf: input,
        pos: 0,
    };
    p.skip_ws();
    let v = p.value(0)?;
    p.skip_ws();
    if p.pos != input.len() {
        return Err(p.err("trailing data after the document"));
    }
    Ok(v)
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn err(&self, why: &'static str) -> JsonError {
        JsonError {
            offset: self.pos,
            why,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn literal(&mut self, word: &'static [u8], v: Value) -> Result<Value, JsonError> {
        if self.buf[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(v)
        } else {
            Err(self.err("invalid literal"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        match self.peek() {
            None => Err(self.err("unexpected end of input")),
            Some(b'n') => self.literal(b"null", Value::Null),
            Some(b't') => self.literal(b"true", Value::Bool(true)),
            Some(b'f') => self.literal(b"false", Value::Bool(false)),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.pos;
        while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
            self.pos += 1;
        }
        // Only ASCII was consumed.
        let text = std::str::from_utf8(&self.buf[start..self.pos])
            .map_err(|_| self.err("invalid number"))?;
        if !is_number_text(text) {
            self.pos = start;
            return Err(self.err("invalid number"));
        }
        Ok(Value::Num(text.to_owned()))
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let digits = self.buf.get(self.pos..self.pos + 4).ok_or_else(|| self.err("short \\u"))?;
        let mut n = 0u32;
        for &d in digits {
            let v = match d {
                b'0'..=b'9' => d - b'0',
                b'a'..=b'f' => d - b'a' + 10,
                b'A'..=b'F' => d - b'A' + 10,
                _ => return Err(self.err("bad \\u escape")),
            };
            n = n << 4 | u32::from(v);
        }
        self.pos += 4;
        Ok(n)
    }

    fn string(&mut self) -> Result<Vec<u8>, JsonError> {
        self.pos += 1; // opening quote
        let mut out = Vec::new();
        loop {
            let Some(b) = self.peek() else {
                return Err(self.err("unterminated string"));
            };
            self.pos += 1;
            match b {
                b'"' => return Ok(out),
                0..=0x1f => {
                    self.pos -= 1;
                    return Err(self.err("control character in string"));
                }
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return Err(self.err("unterminated escape"));
                    };
                    self.pos += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut unit = self.hex4()?;
                            if (0xd800..0xdc00).contains(&unit) {
                                if !self.buf[self.pos..].starts_with(b"\\u") {
                                    return Err(self.err("lone high surrogate"));
                                }
                                self.pos += 2;
                                let low = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&low) {
                                    return Err(self.err("bad surrogate pair"));
                                }
                                unit = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                            }
                            let c =
                                char::from_u32(unit).ok_or_else(|| self.err("bad code point"))?;
                            let mut utf8 = [0u8; 4];
                            out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
                        }
                        _ => return Err(self.err("unknown escape")),
                    }
                }
                _ => out.push(b),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Arr(items));
                }
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.pos += 1;
        let mut members: Vec<(Vec<u8>, Value)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Obj(members));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("expected a key"));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.err("expected ':'"));
            }
            self.pos += 1;
            self.skip_ws();
            let v = self.value(depth + 1)?;
            members.push((key, v));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
        members.sort_by(|a, b| a.0.cmp(&b.0));
        if members.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(self.err("duplicate object key"));
        }
        Ok(Value::Obj(members))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(v: &Value) -> String {
        String::from_utf8(v.to_vec()).unwrap()
    }

    #[test]
    fn numbers_keep_their_text() {
        let v =
            parse(br#"{"a":25000.00000000,"b":-0.00000001,"c":1e-05,"d":0,"e":[1,2.50]}"#).unwrap();
        assert_eq!(enc(&v), r#"{"a":25000.00000000,"b":-0.00000001,"c":1e-05,"d":0,"e":[1,2.50]}"#);
        assert_eq!(v.get(b"a").unwrap().as_num(), Some("25000.00000000"));
    }

    #[test]
    fn object_keys_are_sorted_bytewise() {
        let v = parse(br#"{"b":1,"B":2,"a":3,"":4}"#).unwrap();
        assert_eq!(enc(&v), r#"{"":4,"B":2,"a":3,"b":1}"#);
    }

    #[test]
    fn duplicate_keys_are_refused() {
        assert!(parse(br#"{"a":1,"a":2}"#).is_err());
    }

    #[test]
    fn strings_decode_escapes_to_utf8_bytes() {
        let v = parse(br#""a\"b\\c\/\n\u00e9\ud83d\ude00""#).unwrap();
        assert_eq!(v.as_bytes().unwrap(), "a\"b\\c/\n\u{e9}\u{1f600}".as_bytes());
        // Re-encoded byte-wise by the canonical writer.
        assert_eq!(enc(&v), r#""a\"b\\c/\u000a\u00c3\u00a9\u00f0\u009f\u0098\u0080""#);
    }

    #[test]
    fn bad_input_is_refused() {
        for bad in [
            &b""[..],
            b"[1,]",
            b"{\"a\":}",
            b"01",
            b"1.",
            b"-",
            b"+1",
            b"\"\\ud800\"",
            b"\"\\udc00\"",
            b"\"a\nb\"",
            b"[1] x",
            b"nul",
            b"{'a':1}",
        ] {
            assert!(parse(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn deep_nesting_is_refused() {
        let deep = "[".repeat(MAX_DEPTH + 2);
        assert!(parse(deep.as_bytes()).is_err());
    }

    #[test]
    fn whitespace_and_literals() {
        let v = parse(b" { \"x\" : [ true , false , null ] } \n").unwrap();
        assert_eq!(enc(&v), r#"{"x":[true,false,null]}"#);
    }
}

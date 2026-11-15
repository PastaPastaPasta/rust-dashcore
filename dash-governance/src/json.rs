//! The part of Core's `UniValue` governance reads, over bytes (notes D5, D6).
//!
//! Core reads object data with `UniValue::read`, which builds into the value it is called on and
//! returns `false` part-way through: callers that ignore the result see the partial tree, keys
//! without values included. Strings are bytes after Core's UTF-8 filter, which passes sequences a
//! Rust `String` cannot hold. The getters return Core's exception texts as `Err`.

/// `MAX_JSON_DEPTH`.
// Core: univalue/lib/univalue_read.cpp:20
const MAX_JSON_DEPTH: usize = 512;

/// `UniValue::VType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Null,
    Obj,
    Arr,
    Str,
    Num,
    Bool,
}

impl Kind {
    /// `uvTypeName`.
    // Core: univalue/lib/univalue.cpp:218-231
    fn name(self) -> &'static str {
        match self {
            Kind::Null => "null",
            Kind::Bool => "bool",
            Kind::Obj => "object",
            Kind::Arr => "array",
            Kind::Str => "string",
            Kind::Num => "number",
        }
    }
}

/// A `UniValue`: a type, the text of a string or number, and an object's keys and values.
///
/// A partially read object can hold more keys than values (`{"a":` stops after the key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    kind: Kind,
    val: Vec<u8>,
    keys: Vec<Vec<u8>>,
    values: Vec<Value>,
}

/// `NullUniValue`.
static NULL: Value = Value::null();

impl Value {
    /// `UniValue()`.
    pub const fn null() -> Value {
        Value {
            kind: Kind::Null,
            val: Vec::new(),
            keys: Vec::new(),
            values: Vec::new(),
        }
    }

    /// `UniValue(UniValue::VOBJ)`.
    pub fn empty_object() -> Value {
        Value::of(Kind::Obj)
    }

    fn of(kind: Kind) -> Value {
        Value {
            kind,
            ..Value::null()
        }
    }

    fn scalar(kind: Kind, val: Vec<u8>) -> Value {
        Value {
            kind,
            val,
            ..Value::null()
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn is_object(&self) -> bool {
        self.kind == Kind::Obj
    }

    /// `UniValue::read(raw)` on a fresh value: whether the whole input is one JSON value, and the
    /// value as far as the reader built it.
    // Core: univalue/lib/univalue_read.cpp:259-458
    pub fn read(raw: &[u8]) -> (bool, Value) {
        let mut reader = Reader {
            root: Value::null(),
            stack: Vec::new(),
        };
        let ok = reader.run(raw);
        (ok, reader.finish())
    }

    /// `operator[](key)`: the first value under `key`, or null for a missing key or a value that
    /// is not an object. A key without a value is `values.at(index)`'s `std::out_of_range`.
    // Core: univalue/lib/univalue.cpp:188-198
    pub fn get(&self, key: &[u8]) -> Result<&Value, String> {
        if self.kind != Kind::Obj {
            return Ok(&NULL);
        }
        match self.keys.iter().position(|k| k == key) {
            None => Ok(&NULL),
            Some(index) => {
                self.values.get(index).ok_or_else(|| out_of_range(index, self.values.len()))
            }
        }
    }

    /// `checkType(expected)`.
    // Core: univalue/lib/univalue.cpp:210-216
    fn check(&self, expected: Kind) -> Result<(), String> {
        if self.kind == expected {
            Ok(())
        } else {
            Err(format!(
                "JSON value of type {} is not of expected type {}",
                self.kind.name(),
                expected.name()
            ))
        }
    }

    /// `get_str()`.
    // Core: univalue/lib/univalue_get.cpp:76-80
    pub fn get_str(&self) -> Result<&[u8], String> {
        self.check(Kind::Str)?;
        Ok(&self.val)
    }

    /// `getInt<int64_t>()`: `std::from_chars` over the whole number text.
    // Core: univalue/include/univalue.h:136-147
    pub fn get_i64(&self) -> Result<i64, String> {
        self.check(Kind::Num)?;
        from_chars(&self.val).ok_or_else(|| "JSON integer out of range".to_owned())
    }

    /// `getInt<int>()`.
    pub fn get_i32(&self) -> Result<i32, String> {
        self.check(Kind::Num)?;
        from_chars(&self.val)
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| "JSON integer out of range".to_owned())
    }

    /// `get_real()`.
    // Core: univalue/lib/univalue_get.cpp:82-90
    pub fn get_real(&self) -> Result<f64, String> {
        self.check(Kind::Num)?;
        parse_double(&self.val).ok_or_else(|| "JSON double out of range".to_owned())
    }

    /// `getValues()`.
    // Core: univalue/lib/univalue_get.cpp:56-61
    pub fn get_values(&self) -> Result<&[Value], String> {
        if self.kind != Kind::Obj && self.kind != Kind::Arr {
            return Err("JSON value is not an object or array as expected".to_owned());
        }
        Ok(&self.values)
    }
}

/// libstdc++'s `std::vector::at` exception text (`_M_range_check`), which the release builds
/// throw (notes D5).
pub fn out_of_range(index: usize, size: usize) -> String {
    format!("vector::_M_range_check: __n (which is {index}) >= this->size() (which is {size})")
}

/// `std::from_chars` for a signed integer over the whole of `text`: an optional `-`, then
/// decimal digits, in range.
fn from_chars(text: &[u8]) -> Option<i64> {
    let (negative, digits) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, text),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    // Accumulated negatively so that i64::MIN parses.
    let mut value: i64 = 0;
    for &d in digits {
        value = value.checked_mul(10)?.checked_sub(i64::from(d - b'0'))?;
    }
    if negative {
        Some(value)
    } else {
        value.checked_neg()
    }
}

/// `json_isspace`.
// Core: univalue/include/univalue.h:192-205
fn is_json_space(ch: u8) -> bool {
    matches!(ch, b' ' | b'\t' | b'\n' | b'\r')
}

/// `ParseDouble`: `ParsePrechecks`, no hexadecimal, then libstdc++'s `istringstream >> double`,
/// which fails on an overflow to infinity and not on an underflow (notes D6).
// Core: univalue/lib/univalue_get.cpp:20-44
fn parse_double(text: &[u8]) -> Option<f64> {
    let (&first, &last) = (text.first()?, text.last()?);
    if is_json_space(first) || is_json_space(last) || text.contains(&0) || text.starts_with(b"0x") {
        return None;
    }
    // A number token is ASCII: `-?digits(.digits)?([eE][+-]?digits)?`, or a lone `-`.
    let value: f64 = std::str::from_utf8(text).ok()?.parse().ok()?;
    value.is_finite().then_some(value)
}

/// `jtokentype`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Err,
    None,
    ObjOpen,
    ObjClose,
    ArrOpen,
    ArrClose,
    Colon,
    Comma,
    Null,
    True,
    False,
    Number,
    String,
}

impl Token {
    /// `jsonTokenIsValue`.
    fn is_value(self) -> bool {
        matches!(self, Token::Null | Token::True | Token::False | Token::Number | Token::String)
    }
}

/// The byte at `i`, or the terminator `std::string` keeps after the data.
fn at(raw: &[u8], i: usize) -> u8 {
    raw.get(i).copied().unwrap_or(0)
}

/// `hatoui` over exactly four characters: whether all four are hex digits, and their value.
fn hex4(digits: &[u8]) -> Option<u32> {
    digits.iter().try_fold(0u32, |acc, &c| Some(acc * 16 + char::from(c).to_digit(16)?))
}

/// `getJsonToken`: the token at `pos`, its text and the bytes it consumed.
// Core: univalue/lib/univalue_read.cpp:53-247
fn token(raw: &[u8], start: usize) -> (Token, Vec<u8>, usize) {
    let mut pos = start;
    while pos < raw.len() && is_json_space(raw[pos]) {
        pos += 1;
    }
    if pos >= raw.len() {
        return (Token::None, Vec::new(), 0);
    }
    let simple = |tok: Token| (tok, Vec::new(), pos + 1 - start);
    match raw[pos] {
        b'{' => simple(Token::ObjOpen),
        b'}' => simple(Token::ObjClose),
        b'[' => simple(Token::ArrOpen),
        b']' => simple(Token::ArrClose),
        b':' => simple(Token::Colon),
        b',' => simple(Token::Comma),
        b'n' | b't' | b'f' => {
            // `strncmp` stops at the terminator, which no keyword contains.
            let rest = &raw[pos..];
            for (word, tok) in
                [(&b"null"[..], Token::Null), (b"true", Token::True), (b"false", Token::False)]
            {
                if rest.starts_with(word) {
                    return (tok, Vec::new(), pos + word.len() - start);
                }
            }
            (Token::Err, Vec::new(), 0)
        }
        b'-' | b'0'..=b'9' => {
            let first = pos;
            let first_digit = if raw[first].is_ascii_digit() {
                first
            } else {
                first + 1
            };
            if at(raw, first_digit) == b'0' && at(raw, first_digit + 1).is_ascii_digit() {
                return (Token::Err, Vec::new(), 0);
            }
            pos += 1;
            if raw[first] == b'-' && pos < raw.len() && !raw[pos].is_ascii_digit() {
                return (Token::Err, Vec::new(), 0);
            }
            let digits = |pos: &mut usize| {
                while *pos < raw.len() && raw[*pos].is_ascii_digit() {
                    *pos += 1;
                }
            };
            digits(&mut pos);
            if pos < raw.len() && raw[pos] == b'.' {
                pos += 1;
                if pos >= raw.len() || !raw[pos].is_ascii_digit() {
                    return (Token::Err, Vec::new(), 0);
                }
                digits(&mut pos);
            }
            if pos < raw.len() && (raw[pos] == b'e' || raw[pos] == b'E') {
                pos += 1;
                if pos < raw.len() && (raw[pos] == b'-' || raw[pos] == b'+') {
                    pos += 1;
                }
                if pos >= raw.len() || !raw[pos].is_ascii_digit() {
                    return (Token::Err, Vec::new(), 0);
                }
                digits(&mut pos);
            }
            (Token::Number, raw[first..pos].to_vec(), pos - start)
        }
        b'"' => {
            pos += 1;
            let mut out = Vec::new();
            let mut writer = Utf8Filter::new(&mut out);
            loop {
                if pos >= raw.len() || raw[pos] < 0x20 {
                    return (Token::Err, Vec::new(), 0);
                }
                match raw[pos] {
                    b'\\' => {
                        pos += 1;
                        if pos >= raw.len() {
                            return (Token::Err, Vec::new(), 0);
                        }
                        match raw[pos] {
                            b'"' => writer.push_back(b'"'),
                            b'\\' => writer.push_back(b'\\'),
                            b'/' => writer.push_back(b'/'),
                            b'b' => writer.push_back(0x08),
                            b'f' => writer.push_back(0x0c),
                            b'n' => writer.push_back(b'\n'),
                            b'r' => writer.push_back(b'\r'),
                            b't' => writer.push_back(b'\t'),
                            b'u' => {
                                // `raw + 1 + 4 >= end`: the four digits and one more byte.
                                if pos + 5 >= raw.len() {
                                    return (Token::Err, Vec::new(), 0);
                                }
                                let Some(codepoint) = hex4(&raw[pos + 1..pos + 5]) else {
                                    return (Token::Err, Vec::new(), 0);
                                };
                                writer.push_back_u(codepoint);
                                pos += 4;
                            }
                            _ => return (Token::Err, Vec::new(), 0),
                        }
                        pos += 1;
                    }
                    b'"' => {
                        pos += 1;
                        break;
                    }
                    ch => {
                        writer.push_back(ch);
                        pos += 1;
                    }
                }
            }
            if !writer.finalize() {
                return (Token::Err, Vec::new(), 0);
            }
            (Token::String, out, pos - start)
        }
        _ => (Token::Err, Vec::new(), 0),
    }
}

/// `JSONUTF8StringFilter`: Core's UTF-8 validation, which passes overlong forms, lead bytes
/// F0-F7 and surrogates (paired across `\u` escapes and UTF-8 sequences alike).
// Core: univalue/include/univalue_utffilter.h:13-115
struct Utf8Filter<'a> {
    out: &'a mut Vec<u8>,
    is_valid: bool,
    codepoint: u32,
    state: u32,
    surpair: u32,
}

impl<'a> Utf8Filter<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        Utf8Filter {
            out,
            is_valid: true,
            codepoint: 0,
            state: 0,
            surpair: 0,
        }
    }

    fn push_back(&mut self, ch: u8) {
        let c = u32::from(ch);
        if self.state == 0 {
            if ch < 0x80 {
                self.out.push(ch);
            } else if ch < 0xc0 {
                self.is_valid = false;
            } else if ch < 0xe0 {
                self.codepoint = (c & 0x1f) << 6;
                self.state = 6;
            } else if ch < 0xf0 {
                self.codepoint = (c & 0x0f) << 12;
                self.state = 12;
            } else if ch < 0xf8 {
                self.codepoint = (c & 0x07) << 18;
                self.state = 18;
            } else {
                self.is_valid = false;
            }
        } else {
            if ch & 0xc0 != 0x80 {
                self.is_valid = false;
            }
            self.state -= 6;
            self.codepoint |= (c & 0x3f) << self.state;
            if self.state == 0 {
                self.push_back_u(self.codepoint);
            }
        }
    }

    fn push_back_u(&mut self, codepoint: u32) {
        if self.state != 0 {
            self.is_valid = false;
        }
        if (0xD800..0xDC00).contains(&codepoint) {
            if self.surpair != 0 {
                self.is_valid = false;
            } else {
                self.surpair = codepoint;
            }
        } else if (0xDC00..0xE000).contains(&codepoint) {
            if self.surpair != 0 {
                self.append(0x10000 | ((self.surpair - 0xD800) << 10) | (codepoint - 0xDC00));
                self.surpair = 0;
            } else {
                self.is_valid = false;
            }
        } else if self.surpair != 0 {
            self.is_valid = false;
        } else {
            self.append(codepoint);
        }
    }

    fn finalize(&mut self) -> bool {
        if self.state != 0 || self.surpair != 0 {
            self.is_valid = false;
        }
        self.is_valid
    }

    // Each byte is the low eight bits of Core's `(char)` cast.
    #[allow(clippy::cast_possible_truncation)]
    fn append(&mut self, cp: u32) {
        if cp <= 0x7f {
            self.out.push(cp as u8);
        } else if cp <= 0x7FF {
            self.out.extend([(0xC0 | (cp >> 6)) as u8, (0x80 | (cp & 0x3F)) as u8]);
        } else if cp <= 0xFFFF {
            self.out.extend([
                (0xE0 | (cp >> 12)) as u8,
                (0x80 | ((cp >> 6) & 0x3F)) as u8,
                (0x80 | (cp & 0x3F)) as u8,
            ]);
        } else if cp <= 0x1FFFFF {
            self.out.extend([
                (0xF0 | (cp >> 18)) as u8,
                (0x80 | ((cp >> 12) & 0x3F)) as u8,
                (0x80 | ((cp >> 6) & 0x3F)) as u8,
                (0x80 | (cp & 0x3F)) as u8,
            ]);
        }
    }
}

/// `expect_bits`.
const EXP_OBJ_NAME: u32 = 1 << 0;
const EXP_COLON: u32 = 1 << 1;
const EXP_ARR_VALUE: u32 = 1 << 2;
const EXP_VALUE: u32 = 1 << 3;
const EXP_NOT_VALUE: u32 = 1 << 4;

/// `UniValue::read`'s state. Core keeps pointers to the open containers inside the tree; here the
/// open containers are owned by the stack and attached to their parent when they close, or when
/// the read stops (an open container is always its parent's last value in Core).
struct Reader {
    root: Value,
    stack: Vec<Value>,
}

impl Reader {
    /// A value outside any container replaces the root (`*this = tmpVal`).
    fn attach(&mut self, value: Value) {
        match self.stack.last_mut() {
            Some(top) => top.values.push(value),
            None => self.root = value,
        }
    }

    /// The tree as Core leaves it: every open container in its parent.
    fn finish(mut self) -> Value {
        while let Some(open) = self.stack.pop() {
            self.attach(open);
        }
        self.root
    }

    // Core: univalue/lib/univalue_read.cpp:259-458
    fn run(&mut self, raw: &[u8]) -> bool {
        let mut expect = 0u32;
        let mut tok = Token::None;
        let mut pos = 0;
        loop {
            let last_tok = tok;
            let (t, val, consumed) = token(raw, pos);
            tok = t;
            if tok == Token::None || tok == Token::Err {
                return false;
            }
            pos += consumed;

            let is_value_open = tok.is_value() || tok == Token::ObjOpen || tok == Token::ArrOpen;
            if expect & EXP_VALUE != 0 {
                if !is_value_open {
                    return false;
                }
                expect &= !EXP_VALUE;
            } else if expect & EXP_ARR_VALUE != 0 {
                if !(is_value_open || tok == Token::ArrClose) {
                    return false;
                }
                expect &= !EXP_ARR_VALUE;
            } else if expect & EXP_OBJ_NAME != 0 {
                if !(tok == Token::ObjClose || tok == Token::String) {
                    return false;
                }
            } else if expect & EXP_COLON != 0 {
                if tok != Token::Colon {
                    return false;
                }
                expect &= !EXP_COLON;
            } else if tok == Token::Colon {
                return false;
            }

            if expect & EXP_NOT_VALUE != 0 {
                if is_value_open {
                    return false;
                }
                expect &= !EXP_NOT_VALUE;
            }

            match tok {
                Token::ObjOpen | Token::ArrOpen => {
                    let kind = if tok == Token::ObjOpen {
                        Kind::Obj
                    } else {
                        Kind::Arr
                    };
                    // At the root this is `setObject()`/`setArray()`, which clears the value.
                    self.stack.push(Value::of(kind));
                    if self.stack.len() > MAX_JSON_DEPTH {
                        return false;
                    }
                    expect |= if kind == Kind::Obj {
                        EXP_OBJ_NAME
                    } else {
                        EXP_ARR_VALUE
                    };
                }
                Token::ObjClose | Token::ArrClose => {
                    let Some(top) = self.stack.last() else {
                        return false;
                    };
                    if last_tok == Token::Comma {
                        return false;
                    }
                    let kind = if tok == Token::ObjClose {
                        Kind::Obj
                    } else {
                        Kind::Arr
                    };
                    if kind != top.kind {
                        return false;
                    }
                    let closed = self.stack.pop().unwrap_or_else(Value::null);
                    self.attach(closed);
                    expect &= !EXP_OBJ_NAME;
                    expect |= EXP_NOT_VALUE;
                }
                Token::Colon => {
                    match self.stack.last() {
                        Some(top) if top.kind == Kind::Obj => {}
                        _ => return false,
                    }
                    expect |= EXP_VALUE;
                }
                Token::Comma => {
                    let Some(top) = self.stack.last() else {
                        return false;
                    };
                    if last_tok == Token::Comma || last_tok == Token::ArrOpen {
                        return false;
                    }
                    expect |= if top.kind == Kind::Obj {
                        EXP_OBJ_NAME
                    } else {
                        EXP_ARR_VALUE
                    };
                }
                // A scalar at the root replaces it (`*this = tmpVal`) and ends the loop, where
                // EXP_NOT_VALUE no longer matters.
                Token::Null | Token::True | Token::False | Token::Number => {
                    self.attach(match tok {
                        Token::Null => Value::null(),
                        // `setBool`: Core's text is "1" or "".
                        Token::True => Value::scalar(Kind::Bool, b"1".to_vec()),
                        Token::False => Value::scalar(Kind::Bool, Vec::new()),
                        _ => Value::scalar(Kind::Num, val),
                    });
                    expect |= EXP_NOT_VALUE;
                }
                Token::String => {
                    if expect & EXP_OBJ_NAME != 0 {
                        // EXP_OBJ_NAME is only set while an object is open.
                        if let Some(top) = self.stack.last_mut() {
                            top.keys.push(val);
                        }
                        expect &= !EXP_OBJ_NAME;
                        expect |= EXP_COLON;
                    } else {
                        self.attach(Value::scalar(Kind::Str, val));
                    }
                    expect |= EXP_NOT_VALUE;
                }
                Token::Err | Token::None => return false,
            }
            if self.stack.is_empty() {
                break;
            }
        }
        // Nothing may follow the value.
        token(raw, pos).0 == Token::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(s: &str) -> (bool, Value) {
        Value::read(s.as_bytes())
    }

    #[test]
    fn whole_values() {
        assert!(read(r#"{"a":1,"b":[true,null,"x"]}"#).0);
        assert!(read(" 1 ").0);
        assert!(read(r#""s""#).0);
        assert!(!read("1 2").0);
        assert!(!read("").0);
        assert!(!read("{,}").0);
        assert!(!read("[1,]").0);
        assert!(!read("01").0);
        assert!(!read("-01").0);
        assert!(!read("1.").0);
        assert!(!read("1e").0);
        assert!(!read("nul").0);
    }

    #[test]
    fn partial_root_is_kept() {
        let (ok, v) = read(r#"{"type":1,"name":"x""#);
        assert!(!ok);
        assert!(v.is_object());
        assert_eq!(v.get(b"type").unwrap().get_i64(), Ok(1));
        assert_eq!(v.get(b"name").unwrap().get_str(), Ok(&b"x"[..]));

        // A key without its value: `values.at(1)` throws.
        let (_, v) = read(r#"{"type":1,"name":"#);
        assert_eq!(v.get(b"name"), Err(out_of_range(1, 1)));

        // A failure before any token leaves null.
        let (_, v) = read("x");
        assert_eq!(v.kind(), Kind::Null);

        // An open nested container stays in its parent.
        let (_, v) = read(r#"{"a":{"b":2"#);
        assert_eq!(v.get(b"a").unwrap().get(b"b").unwrap().get_i64(), Ok(2));
    }

    #[test]
    fn lone_minus_is_a_number_at_the_end() {
        let (ok, v) = read("-");
        assert!(ok);
        assert_eq!(v.kind(), Kind::Num);
        assert_eq!(v.get_i64(), Err("JSON integer out of range".to_owned()));
        assert_eq!(v.get_real(), Err("JSON double out of range".to_owned()));
        assert!(!read("-x").0);
    }

    #[test]
    fn getters_report_core_texts() {
        let (_, v) = read(r#"{"n":1e400,"m":1e-400,"s":"x","big":99999999999999999999}"#);
        assert_eq!(v.get(b"n").unwrap().get_real(), Err("JSON double out of range".to_owned()));
        assert_eq!(v.get(b"m").unwrap().get_real(), Ok(0.0));
        assert_eq!(
            v.get(b"s").unwrap().get_i64(),
            Err("JSON value of type string is not of expected type number".to_owned())
        );
        assert_eq!(
            v.get(b"missing").unwrap().get_str(),
            Err("JSON value of type null is not of expected type string".to_owned())
        );
        assert_eq!(v.get(b"big").unwrap().get_i64(), Err("JSON integer out of range".to_owned()));
        assert_eq!(v.get(b"n").unwrap().get_i32(), Err("JSON integer out of range".to_owned()));
        assert_eq!(
            read("1").1.get_values(),
            Err("JSON value is not an object or array as expected".to_owned())
        );
        assert_eq!(read("-9223372036854775808").1.get_i64(), Ok(i64::MIN));
        assert_eq!(read("2147483648").1.get_i32(), Err("JSON integer out of range".to_owned()));
    }

    #[test]
    fn strings_are_filtered_bytes() {
        // Overlong NUL, a code point above U+10FFFF, and a surrogate pair from UTF-8 bytes.
        let (ok, v) = Value::read(b"[\"\xc0\x80\xf7\xbf\xbf\xbf\xed\xa0\x80\xed\xb0\x80\"]");
        assert!(ok);
        assert_eq!(
            v.get_values().unwrap()[0].get_str(),
            Ok(&b"\0\xf7\xbf\xbf\xbf\xf0\x90\x80\x80"[..])
        );
        assert!(read(r#"["𝄞"]"#).0);
        assert!(!read(r#"["\ud834"]"#).0);
        assert!(!Value::read(b"[\"\x80\"]").0);
        assert!(!Value::read(b"[\"\x01\"]").0);
        // `\u` needs a byte after its four digits.
        assert!(!read(r#""A"#).0);
    }

    #[test]
    fn depth_limit() {
        let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert!(read(&deep(MAX_JSON_DEPTH)).0);
        assert!(!read(&deep(MAX_JSON_DEPTH + 1)).0);
    }
}

//! The subset of Core's `UniValue` that `util::SettingsValue` uses.
//!
//! Core stores every setting as a `UniValue`. Its JSON reader and writer decide what
//! `settings.json` accepts and how it is written back, and its strict getters decide which
//! settings make the `GetArg` family throw. This module ports them:
//! - [`SettingsValue::read`] is `UniValue::read` (`univalue_read.cpp`), including its UTF-8
//!   filter, its depth limit and its acceptance of duplicate object keys;
//! - [`SettingsValue::write`] is `UniValue::write` (`univalue_write.cpp`), with Core's escape
//!   table;
//! - numbers keep their text (`VNUM` holds a string), so `1.50` reads and writes back unchanged.
//!
//! Core: src/univalue/{include/univalue.h,lib/univalue*.cpp} at v24.0.0-rc.3.

use std::fmt;

/// A setting value (Core: `util::SettingsValue`, a `UniValue`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsValue {
    /// `VNULL`: unset.
    Null,
    /// `VBOOL`. Core represents a negated option (`-nofoo`) as `false`.
    Bool(bool),
    /// `VNUM`, holding the number's text exactly as read or formatted.
    Num(String),
    /// `VSTR`.
    Str(String),
    /// `VARR`.
    Arr(Vec<SettingsValue>),
    /// `VOBJ`: keys in insertion order. Duplicate keys are kept, as Core's reader keeps them.
    Obj(Vec<(String, SettingsValue)>),
}

/// Core's `UniValue::VType`, used in the strict getters' error messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    Null,
    Bool,
    Obj,
    Arr,
    Str,
    Num,
}

impl ValueType {
    /// Core: `uvTypeName`.
    pub fn name(self) -> &'static str {
        match self {
            ValueType::Null => "null",
            ValueType::Bool => "bool",
            ValueType::Obj => "object",
            ValueType::Arr => "array",
            ValueType::Str => "string",
            ValueType::Num => "number",
        }
    }
}

/// The `std::runtime_error` Core's strict `UniValue` getters throw. Its `Display` is Core's
/// `what()` text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeError(pub String);

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RuntimeError {}

impl SettingsValue {
    /// An empty object (Core: `UniValue(UniValue::VOBJ)`).
    pub fn new_object() -> Self {
        SettingsValue::Obj(Vec::new())
    }

    /// An empty array (Core: `UniValue(UniValue::VARR)`).
    pub fn new_array() -> Self {
        SettingsValue::Arr(Vec::new())
    }

    /// A number from its JSON text, or `None` if Core's `validNumStr` would refuse it.
    pub fn from_num_str(text: &str) -> Option<Self> {
        match get_json_token(text.as_bytes()) {
            Some((Token::Number(num), consumed)) if consumed == text.len() => {
                Some(SettingsValue::Num(num))
            }
            _ => None,
        }
    }

    /// Core: `UniValue::setFloat`, which formats with `std::setprecision(16)` (`%.16g`) and, as
    /// `setNumStr`, refuses text that is not a JSON number: infinities and NaN cannot be stored.
    pub fn from_f64(value: f64) -> Result<Self, RuntimeError> {
        let text = format_g16(value);
        SettingsValue::from_num_str(&text)
            .ok_or_else(|| RuntimeError(format!("The string '{text}' is not a valid JSON number")))
    }

    pub fn value_type(&self) -> ValueType {
        match self {
            SettingsValue::Null => ValueType::Null,
            SettingsValue::Bool(_) => ValueType::Bool,
            SettingsValue::Num(_) => ValueType::Num,
            SettingsValue::Str(_) => ValueType::Str,
            SettingsValue::Arr(_) => ValueType::Arr,
            SettingsValue::Obj(_) => ValueType::Obj,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, SettingsValue::Null)
    }
    pub fn is_bool(&self) -> bool {
        matches!(self, SettingsValue::Bool(_))
    }
    pub fn is_true(&self) -> bool {
        matches!(self, SettingsValue::Bool(true))
    }
    pub fn is_false(&self) -> bool {
        matches!(self, SettingsValue::Bool(false))
    }
    pub fn is_num(&self) -> bool {
        matches!(self, SettingsValue::Num(_))
    }
    pub fn is_str(&self) -> bool {
        matches!(self, SettingsValue::Str(_))
    }
    pub fn is_array(&self) -> bool {
        matches!(self, SettingsValue::Arr(_))
    }
    pub fn is_object(&self) -> bool {
        matches!(self, SettingsValue::Obj(_))
    }

    fn check_type(&self, expected: ValueType) -> Result<(), RuntimeError> {
        if self.value_type() == expected {
            Ok(())
        } else {
            Err(RuntimeError(format!(
                "JSON value of type {} is not of expected type {}",
                self.value_type().name(),
                expected.name()
            )))
        }
    }

    /// Core: `UniValue::get_str`.
    pub fn get_str(&self) -> Result<&str, RuntimeError> {
        self.check_type(ValueType::Str)?;
        match self {
            SettingsValue::Str(s) => Ok(s),
            _ => unreachable!(),
        }
    }

    /// Core: `UniValue::get_bool`.
    pub fn get_bool(&self) -> Result<bool, RuntimeError> {
        self.check_type(ValueType::Bool)?;
        Ok(self.is_true())
    }

    /// Core: `UniValue::getInt<int64_t>`: `std::from_chars` over the whole number text.
    pub fn get_int64(&self) -> Result<i64, RuntimeError> {
        self.check_type(ValueType::Num)?;
        let text = self.get_val_str();
        match from_chars_i64(text.as_bytes()) {
            (FromChars::Ok(value), consumed) if consumed == text.len() => Ok(value),
            _ => Err(RuntimeError("JSON integer out of range".to_string())),
        }
    }

    /// Core: `UniValue::getValStr`: the stored text (`"1"`/`""` for booleans, empty for
    /// containers and null).
    pub fn get_val_str(&self) -> &str {
        match self {
            SettingsValue::Bool(true) => "1",
            SettingsValue::Num(s) | SettingsValue::Str(s) => s,
            _ => "",
        }
    }

    /// Core: `UniValue::getValues`.
    pub fn get_values(&self) -> Result<Vec<&SettingsValue>, RuntimeError> {
        match self {
            SettingsValue::Arr(values) => Ok(values.iter().collect()),
            SettingsValue::Obj(entries) => Ok(entries.iter().map(|(_, v)| v).collect()),
            _ => Err(RuntimeError("JSON value is not an object or array as expected".to_string())),
        }
    }

    /// Core: `UniValue::push_back` (on an array).
    pub fn push_back(&mut self, value: SettingsValue) {
        match self {
            SettingsValue::Arr(values) => values.push(value),
            other => panic!("push_back on a {} value", other.value_type().name()),
        }
    }

    /// Core: `UniValue::pushKVEnd` (on an object).
    pub fn push_kv_end(&mut self, key: impl Into<String>, value: SettingsValue) {
        match self {
            SettingsValue::Obj(entries) => entries.push((key.into(), value)),
            other => panic!("pushKVEnd on a {} value", other.value_type().name()),
        }
    }

    /// Core: `UniValue::write(0, 0)`.
    pub fn write(&self) -> String {
        self.write_pretty(0, 0)
    }

    /// Core: `UniValue::write(prettyIndent, indentLevel)`.
    pub fn write_pretty(&self, pretty_indent: usize, indent_level: usize) -> String {
        let mut s = String::new();
        self.write_into(pretty_indent, indent_level, &mut s);
        s
    }

    fn write_into(&self, pretty_indent: usize, indent_level: usize, s: &mut String) {
        let mod_indent = if indent_level == 0 {
            1
        } else {
            indent_level
        };
        match self {
            SettingsValue::Null => s.push_str("null"),
            SettingsValue::Obj(entries) => {
                s.push('{');
                if pretty_indent > 0 {
                    s.push('\n');
                }
                for (i, (key, value)) in entries.iter().enumerate() {
                    if pretty_indent > 0 {
                        indent_str(pretty_indent, mod_indent, s);
                    }
                    s.push('"');
                    json_escape(key, s);
                    s.push_str("\":");
                    if pretty_indent > 0 {
                        s.push(' ');
                    }
                    value.write_into(pretty_indent, mod_indent + 1, s);
                    if i != entries.len() - 1 {
                        s.push(',');
                    }
                    if pretty_indent > 0 {
                        s.push('\n');
                    }
                }
                if pretty_indent > 0 {
                    indent_str(pretty_indent, mod_indent - 1, s);
                }
                s.push('}');
            }
            SettingsValue::Arr(values) => {
                s.push('[');
                if pretty_indent > 0 {
                    s.push('\n');
                }
                for (i, value) in values.iter().enumerate() {
                    if pretty_indent > 0 {
                        indent_str(pretty_indent, mod_indent, s);
                    }
                    value.write_into(pretty_indent, mod_indent + 1, s);
                    if i != values.len() - 1 {
                        s.push(',');
                    }
                    if pretty_indent > 0 {
                        s.push('\n');
                    }
                }
                if pretty_indent > 0 {
                    indent_str(pretty_indent, mod_indent - 1, s);
                }
                s.push(']');
            }
            SettingsValue::Str(value) => {
                s.push('"');
                json_escape(value, s);
                s.push('"');
            }
            SettingsValue::Num(text) => s.push_str(text),
            SettingsValue::Bool(b) => s.push_str(if *b {
                "true"
            } else {
                "false"
            }),
        }
    }

    /// Core: `UniValue::read`. Returns `None` where Core returns `false`.
    ///
    /// Core builds strings byte by byte through its UTF-8 filter, which accepts a few
    /// sequences that are not valid UTF-8 (code points above U+10FFFF from `F4`–`F7` lead
    /// bytes). Such a document is refused here, since a Rust `String` cannot hold it.
    pub fn read(raw: &[u8]) -> Option<SettingsValue> {
        const MAX_JSON_DEPTH: usize = 512;

        // Containers under construction; the root is `root` once the first token is read.
        let mut stack: Vec<SettingsValue> = Vec::new();
        // An object's key waiting for its value, per stack entry.
        let mut pending_keys: Vec<Option<String>> = Vec::new();
        let mut root: Option<SettingsValue> = None;
        let mut expect = 0u32;
        let mut tok = TokenKind::None;
        let mut pos = 0usize;

        const EXP_OBJ_NAME: u32 = 1 << 0;
        const EXP_COLON: u32 = 1 << 1;
        const EXP_ARR_VALUE: u32 = 1 << 2;
        const EXP_VALUE: u32 = 1 << 3;
        const EXP_NOT_VALUE: u32 = 1 << 4;

        // Attach a finished value to the container on top of the stack.
        fn attach(
            stack: &mut [SettingsValue],
            pending_keys: &mut [Option<String>],
            value: SettingsValue,
        ) {
            let top = stack.last_mut().expect("non-empty stack");
            match top {
                SettingsValue::Arr(values) => values.push(value),
                SettingsValue::Obj(entries) => {
                    // Core pushes keys and values onto separate vectors; a value without a
                    // key cannot happen because the expect bits require a key and colon.
                    let key = pending_keys.last_mut().and_then(Option::take).unwrap_or_default();
                    entries.push((key, value));
                }
                _ => unreachable!(),
            }
        }

        loop {
            let last_tok = tok;
            let (token, consumed) = get_json_token(&raw[pos..])?;
            tok = token.kind();
            pos += consumed;
            if tok == TokenKind::None {
                return None; // eof before the construct closed
            }

            let is_value_open =
                token.is_value() || matches!(tok, TokenKind::ObjOpen | TokenKind::ArrOpen);

            if expect & EXP_VALUE != 0 {
                if !is_value_open {
                    return None;
                }
                expect &= !EXP_VALUE;
            } else if expect & EXP_ARR_VALUE != 0 {
                let is_arr_value = is_value_open || tok == TokenKind::ArrClose;
                if !is_arr_value {
                    return None;
                }
                expect &= !EXP_ARR_VALUE;
            } else if expect & EXP_OBJ_NAME != 0 {
                let is_obj_name = tok == TokenKind::ObjClose || tok == TokenKind::Str;
                if !is_obj_name {
                    return None;
                }
            } else if expect & EXP_COLON != 0 {
                if tok != TokenKind::Colon {
                    return None;
                }
                expect &= !EXP_COLON;
            } else if tok == TokenKind::Colon {
                return None;
            }

            if expect & EXP_NOT_VALUE != 0 {
                if is_value_open {
                    return None;
                }
                expect &= !EXP_NOT_VALUE;
            }

            match token {
                Token::ObjOpen | Token::ArrOpen => {
                    let is_obj = matches!(token, Token::ObjOpen);
                    stack.push(if is_obj {
                        SettingsValue::new_object()
                    } else {
                        SettingsValue::new_array()
                    });
                    pending_keys.push(None);
                    if stack.len() > MAX_JSON_DEPTH {
                        return None;
                    }
                    if is_obj {
                        expect |= EXP_OBJ_NAME;
                    } else {
                        expect |= EXP_ARR_VALUE;
                    }
                }
                Token::ObjClose | Token::ArrClose => {
                    if stack.is_empty() || last_tok == TokenKind::Comma {
                        return None;
                    }
                    let is_obj = matches!(token, Token::ObjClose);
                    if is_obj != stack.last().is_some_and(SettingsValue::is_object) {
                        return None;
                    }
                    let finished = stack.pop().expect("non-empty stack");
                    pending_keys.pop();
                    if stack.is_empty() {
                        root = Some(finished);
                    } else {
                        attach(&mut stack, &mut pending_keys, finished);
                    }
                    expect &= !EXP_OBJ_NAME;
                    expect |= EXP_NOT_VALUE;
                }
                Token::Colon => {
                    if !stack.last().is_some_and(SettingsValue::is_object) {
                        return None;
                    }
                    expect |= EXP_VALUE;
                }
                Token::Comma => {
                    if stack.is_empty()
                        || last_tok == TokenKind::Comma
                        || last_tok == TokenKind::ArrOpen
                    {
                        return None;
                    }
                    if stack.last().is_some_and(SettingsValue::is_object) {
                        expect |= EXP_OBJ_NAME;
                    } else {
                        expect |= EXP_ARR_VALUE;
                    }
                }
                Token::Null | Token::True | Token::False | Token::Number(_) => {
                    let value = match token {
                        Token::Null => SettingsValue::Null,
                        Token::True => SettingsValue::Bool(true),
                        Token::False => SettingsValue::Bool(false),
                        Token::Number(n) => SettingsValue::Num(n),
                        _ => unreachable!(),
                    };
                    if stack.is_empty() {
                        root = Some(value);
                    } else {
                        attach(&mut stack, &mut pending_keys, value);
                        expect |= EXP_NOT_VALUE;
                    }
                }
                Token::Str(s) => {
                    if expect & EXP_OBJ_NAME != 0 {
                        *pending_keys.last_mut().expect("object on the stack") = Some(s);
                        expect &= !EXP_OBJ_NAME;
                        expect |= EXP_COLON;
                    } else if stack.is_empty() {
                        root = Some(SettingsValue::Str(s));
                    } else {
                        attach(&mut stack, &mut pending_keys, SettingsValue::Str(s));
                    }
                    expect |= EXP_NOT_VALUE;
                }
                Token::None => unreachable!("handled above"),
            }
            if stack.is_empty() {
                break;
            }
        }

        // Nothing may follow the initial construct.
        match get_json_token(&raw[pos..]) {
            Some((Token::None, _)) => root,
            _ => None,
        }
    }
}

impl From<&str> for SettingsValue {
    fn from(s: &str) -> Self {
        SettingsValue::Str(s.to_string())
    }
}

impl From<String> for SettingsValue {
    fn from(s: String) -> Self {
        SettingsValue::Str(s)
    }
}

impl From<bool> for SettingsValue {
    fn from(b: bool) -> Self {
        SettingsValue::Bool(b)
    }
}

impl From<i64> for SettingsValue {
    fn from(n: i64) -> Self {
        SettingsValue::Num(n.to_string())
    }
}

impl From<i32> for SettingsValue {
    fn from(n: i32) -> Self {
        SettingsValue::Num(n.to_string())
    }
}

impl From<u64> for SettingsValue {
    fn from(n: u64) -> Self {
        SettingsValue::Num(n.to_string())
    }
}

impl fmt::Display for SettingsValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.write())
    }
}

fn indent_str(pretty_indent: usize, indent_level: usize, s: &mut String) {
    s.extend(std::iter::repeat_n(' ', pretty_indent * indent_level));
}

/// Core: `json_escape` with `univalue_escapes.h`: control characters, `"`, `\` and DEL are
/// escaped; every other byte is copied.
fn json_escape(input: &str, out: &mut String) {
    for ch in input.chars() {
        match ch {
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenKind {
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
    Str,
}

#[derive(Debug)]
enum Token {
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
    Number(String),
    Str(String),
}

impl Token {
    fn kind(&self) -> TokenKind {
        match self {
            Token::None => TokenKind::None,
            Token::ObjOpen => TokenKind::ObjOpen,
            Token::ObjClose => TokenKind::ObjClose,
            Token::ArrOpen => TokenKind::ArrOpen,
            Token::ArrClose => TokenKind::ArrClose,
            Token::Colon => TokenKind::Colon,
            Token::Comma => TokenKind::Comma,
            Token::Null => TokenKind::Null,
            Token::True => TokenKind::True,
            Token::False => TokenKind::False,
            Token::Number(_) => TokenKind::Number,
            Token::Str(_) => TokenKind::Str,
        }
    }

    /// Core: `jsonTokenIsValue`.
    fn is_value(&self) -> bool {
        matches!(self, Token::Null | Token::True | Token::False | Token::Number(_) | Token::Str(_))
    }
}

fn json_isspace(ch: u8) -> bool {
    matches!(ch, 0x20 | 0x09 | 0x0a | 0x0d)
}

/// Core: `getJsonToken`. Returns the token and the bytes consumed, or `None` for `JTOK_ERR`.
fn get_json_token(raw: &[u8]) -> Option<(Token, usize)> {
    let mut i = 0;
    while i < raw.len() && json_isspace(raw[i]) {
        i += 1;
    }
    if i >= raw.len() {
        return Some((Token::None, 0));
    }
    let at = |j: usize| raw.get(j).copied().unwrap_or(0);
    let single = |t: Token| Some((t, i + 1));
    match raw[i] {
        b'{' => single(Token::ObjOpen),
        b'}' => single(Token::ObjClose),
        b'[' => single(Token::ArrOpen),
        b']' => single(Token::ArrClose),
        b':' => single(Token::Colon),
        b',' => single(Token::Comma),
        b'n' | b't' | b'f' => {
            let rest = &raw[i..];
            if rest.starts_with(b"null") {
                Some((Token::Null, i + 4))
            } else if rest.starts_with(b"true") {
                Some((Token::True, i + 4))
            } else if rest.starts_with(b"false") {
                Some((Token::False, i + 5))
            } else {
                None
            }
        }
        b'-' | b'0'..=b'9' => {
            let first = i;
            let first_digit = if at(first).is_ascii_digit() {
                first
            } else {
                first + 1
            };
            if at(first_digit) == b'0' && at(first_digit + 1).is_ascii_digit() {
                return None;
            }
            let mut j = first + 1;
            if raw[first] == b'-' && j < raw.len() && !raw[j].is_ascii_digit() {
                return None;
            }
            while j < raw.len() && raw[j].is_ascii_digit() {
                j += 1;
            }
            if j < raw.len() && raw[j] == b'.' {
                j += 1;
                if j >= raw.len() || !raw[j].is_ascii_digit() {
                    return None;
                }
                while j < raw.len() && raw[j].is_ascii_digit() {
                    j += 1;
                }
            }
            if j < raw.len() && (raw[j] == b'e' || raw[j] == b'E') {
                j += 1;
                if j < raw.len() && (raw[j] == b'-' || raw[j] == b'+') {
                    j += 1;
                }
                if j >= raw.len() || !raw[j].is_ascii_digit() {
                    return None;
                }
                while j < raw.len() && raw[j].is_ascii_digit() {
                    j += 1;
                }
            }
            let text = std::str::from_utf8(&raw[first..j]).expect("ASCII number");
            Some((Token::Number(text.to_string()), j))
        }
        b'"' => {
            let mut j = i + 1;
            let mut writer = Utf8Filter::default();
            loop {
                if j >= raw.len() || raw[j] < 0x20 {
                    return None;
                }
                if raw[j] == b'\\' {
                    j += 1;
                    if j >= raw.len() {
                        return None;
                    }
                    match raw[j] {
                        b'"' => writer.push_back(b'"'),
                        b'\\' => writer.push_back(b'\\'),
                        b'/' => writer.push_back(b'/'),
                        b'b' => writer.push_back(0x08),
                        b'f' => writer.push_back(0x0c),
                        b'n' => writer.push_back(b'\n'),
                        b'r' => writer.push_back(b'\r'),
                        b't' => writer.push_back(b'\t'),
                        b'u' => {
                            // Core: `raw + 1 + 4 >= end` refuses an escape that ends the input.
                            if j + 1 + 4 >= raw.len() {
                                return None;
                            }
                            let mut codepoint = 0u32;
                            for &h in &raw[j + 1..j + 5] {
                                let digit = match h {
                                    b'0'..=b'9' => h - b'0',
                                    b'a'..=b'f' => h - b'a' + 10,
                                    b'A'..=b'F' => h - b'A' + 10,
                                    _ => return None,
                                };
                                codepoint = 16 * codepoint + u32::from(digit);
                            }
                            writer.push_back_u(codepoint);
                            j += 4;
                        }
                        _ => return None,
                    }
                    j += 1;
                } else if raw[j] == b'"' {
                    j += 1;
                    break;
                } else {
                    writer.push_back(raw[j]);
                    j += 1;
                }
            }
            let bytes = writer.finalize()?;
            Some((Token::Str(String::from_utf8(bytes).ok()?), j))
        }
        _ => None,
    }
}

/// Core: `JSONUTF8StringFilter`.
#[derive(Default)]
struct Utf8Filter {
    out: Vec<u8>,
    invalid: bool,
    codepoint: u32,
    state: u32,
    surpair: u32,
}

impl Utf8Filter {
    fn push_back(&mut self, ch: u8) {
        if self.state == 0 {
            if ch < 0x80 {
                self.out.push(ch);
            } else if ch < 0xc0 {
                self.invalid = true;
            } else if ch < 0xe0 {
                self.codepoint = u32::from(ch & 0x1f) << 6;
                self.state = 6;
            } else if ch < 0xf0 {
                self.codepoint = u32::from(ch & 0x0f) << 12;
                self.state = 12;
            } else if ch < 0xf8 {
                self.codepoint = u32::from(ch & 0x07) << 18;
                self.state = 18;
            } else {
                self.invalid = true;
            }
        } else {
            if ch & 0xc0 != 0x80 {
                self.invalid = true;
            }
            self.state -= 6;
            self.codepoint |= u32::from(ch & 0x3f) << self.state;
            if self.state == 0 {
                self.push_back_u(self.codepoint);
            }
        }
    }

    fn push_back_u(&mut self, codepoint: u32) {
        if self.state != 0 {
            self.invalid = true;
        }
        if (0xD800..0xDC00).contains(&codepoint) {
            if self.surpair != 0 {
                self.invalid = true;
            } else {
                self.surpair = codepoint;
            }
        } else if (0xDC00..0xE000).contains(&codepoint) {
            if self.surpair != 0 {
                self.append_codepoint(
                    0x10000 | ((self.surpair - 0xD800) << 10) | (codepoint - 0xDC00),
                );
                self.surpair = 0;
            } else {
                self.invalid = true;
            }
        } else if self.surpair != 0 {
            self.invalid = true;
        } else {
            self.append_codepoint(codepoint);
        }
    }

    fn append_codepoint(&mut self, cp: u32) {
        if cp <= 0x7f {
            self.out.push(cp as u8);
        } else if cp <= 0x7FF {
            self.out.push((0xC0 | (cp >> 6)) as u8);
            self.out.push((0x80 | (cp & 0x3F)) as u8);
        } else if cp <= 0xFFFF {
            self.out.push((0xE0 | (cp >> 12)) as u8);
            self.out.push((0x80 | ((cp >> 6) & 0x3F)) as u8);
            self.out.push((0x80 | (cp & 0x3F)) as u8);
        } else if cp <= 0x1FFFFF {
            self.out.push((0xF0 | (cp >> 18)) as u8);
            self.out.push((0x80 | ((cp >> 12) & 0x3F)) as u8);
            self.out.push((0x80 | ((cp >> 6) & 0x3F)) as u8);
            self.out.push((0x80 | (cp & 0x3F)) as u8);
        }
    }

    /// The decoded bytes, or `None` where Core's `finalize()` returns false.
    fn finalize(mut self) -> Option<Vec<u8>> {
        if self.state != 0 || self.surpair != 0 {
            self.invalid = true;
        }
        (!self.invalid).then_some(self.out)
    }
}

/// The outcome of `std::from_chars` for a signed integer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FromChars {
    Ok(i64),
    /// `std::errc::invalid_argument`: no digits.
    Invalid,
    /// `std::errc::result_out_of_range`.
    OutOfRange,
}

/// `std::from_chars(first, last, int64_t&)`: an optional `-` and decimal digits, stopping at the
/// first non-digit. Returns the outcome and the bytes consumed.
pub(crate) fn from_chars_i64(s: &[u8]) -> (FromChars, usize) {
    let negative = s.first() == Some(&b'-');
    let start = usize::from(negative);
    let mut end = start;
    while end < s.len() && s[end].is_ascii_digit() {
        end += 1;
    }
    if end == start {
        return (FromChars::Invalid, 0);
    }
    let mut value: i128 = 0;
    let mut overflow = false;
    for &d in &s[start..end] {
        value = value * 10 + i128::from(d - b'0');
        if value > i128::from(i64::MAX) + 1 {
            overflow = true;
            value = i128::from(i64::MAX) + 2;
        }
    }
    let value = if negative {
        -value
    } else {
        value
    };
    if overflow || value > i128::from(i64::MAX) || value < i128::from(i64::MIN) {
        return (FromChars::OutOfRange, end);
    }
    (FromChars::Ok(value as i64), end)
}

/// `std::ostream << std::setprecision(16) << value` (the `%.16g` conversion).
fn format_g16(value: f64) -> String {
    const PRECISION: i32 = 16;
    if value.is_nan() {
        // glibc prints a negative NaN's sign; the MinGW build of Core's Windows releases does
        // not (gen/univalue_float_probe.cpp).
        return if value.is_sign_negative() && cfg!(not(windows)) {
            "-nan"
        } else {
            "nan"
        }
        .to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 {
            "-inf"
        } else {
            "inf"
        }
        .to_string();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0"
        } else {
            "0"
        }
        .to_string();
    }
    // Round to PRECISION significant digits first; the exponent of the rounded value decides
    // between fixed and scientific notation.
    let sci = format!("{:.*e}", (PRECISION - 1) as usize, value);
    let (mantissa, exp) = sci.split_once('e').expect("exponent");
    let exp: i32 = exp.parse().expect("exponent digits");
    let strip = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if !(-4..PRECISION).contains(&exp) {
        let sign = if exp < 0 {
            '-'
        } else {
            '+'
        };
        format!("{}e{}{:02}", strip(mantissa), sign, exp.abs())
    } else {
        let decimals = (PRECISION - 1 - exp).max(0) as usize;
        strip(&format!("{:.*}", decimals, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(s: &str) -> Option<SettingsValue> {
        SettingsValue::read(s.as_bytes())
    }

    fn float(value: f64) -> SettingsValue {
        SettingsValue::from_f64(value).unwrap()
    }

    #[test]
    fn float_formatting_matches_setprecision_16() {
        assert_eq!(float(3.25).write(), "3.25");
        assert_eq!(float(0.1).write(), "0.1");
        assert_eq!(float(1e20).write(), "1e+20");
        assert_eq!(float(1.5e-5).write(), "1.5e-05");
        assert_eq!(float(123456789012345678.0).write(), "1.234567890123457e+17");
        assert_eq!(float(100.0).write(), "100");
    }

    // Core's texts, from `UniValue::setNumStr`, as gen/univalue_float_probe.cpp printed them
    // built for Linux (glibc) and for Windows (MinGW-w64 g++ 13, under Wine), which differ only
    // in the negative NaN.
    #[test]
    fn non_finite_floats_are_refused() {
        let negative_nan = if cfg!(windows) {
            "nan"
        } else {
            "-nan"
        };
        for (value, text) in [
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
            (-f64::NAN, negative_nan),
        ] {
            assert_eq!(
                SettingsValue::from_f64(value).unwrap_err().0,
                format!("The string '{text}' is not a valid JSON number")
            );
        }
        assert_eq!(float(f64::MAX).write(), "1.797693134862316e+308");
        assert_eq!(float(-0.0).write(), "-0");
    }

    #[test]
    fn reader_matches_univalue_quirks() {
        // A lone minus sign is a number to UniValue's tokenizer.
        assert_eq!(read("-"), Some(SettingsValue::Num("-".into())));
        assert_eq!(read("01"), None);
        assert_eq!(read("1.50"), Some(SettingsValue::Num("1.50".into())));
        assert_eq!(
            read(" {\"a\":1,\"a\":2} "),
            Some(SettingsValue::Obj(vec![
                ("a".into(), SettingsValue::Num("1".into())),
                ("a".into(), SettingsValue::Num("2".into())),
            ]))
        );
        assert_eq!(read("[1,]"), None);
        assert_eq!(read("[,1]"), None);
        assert_eq!(read("{} x"), None);
        assert_eq!(read("\"\\ud834\\udd1e\""), Some(SettingsValue::Str("\u{1d11e}".into())));
        assert_eq!(read("\"\\ud834\""), None);
        assert_eq!(read("\"\\u0041"), None);
        assert_eq!(read(""), None);
        assert_eq!(read(&"[".repeat(512).to_string().chain_close(512)), Some(nested(512)));
        assert_eq!(read(&"[".repeat(513).to_string().chain_close(513)), None);
    }

    trait ChainClose {
        fn chain_close(self, n: usize) -> String;
    }

    impl ChainClose for String {
        fn chain_close(self, n: usize) -> String {
            self + &"]".repeat(n)
        }
    }

    fn nested(n: usize) -> SettingsValue {
        let mut v = SettingsValue::new_array();
        for _ in 1..n {
            v = SettingsValue::Arr(vec![v]);
        }
        v
    }

    #[test]
    fn writer_escapes_like_core() {
        let v = SettingsValue::from("a\"b\\c\u{1}\u{7f}é\n");
        assert_eq!(v.write(), "\"a\\\"b\\\\c\\u0001\\u007fé\\n\"");
        let mut obj = SettingsValue::new_object();
        obj.push_kv_end("k", SettingsValue::Arr(vec![1i64.into(), SettingsValue::new_object()]));
        assert_eq!(
            obj.write_pretty(4, 1),
            "{\n    \"k\": [\n        1,\n        {\n        }\n    ]\n}"
        );
    }

    #[test]
    fn strict_getters_throw_core_messages() {
        let v = SettingsValue::from(99i64);
        assert_eq!(
            v.get_str().unwrap_err().0,
            "JSON value of type number is not of expected type string"
        );
        assert_eq!(float(3.25).get_int64().unwrap_err().0, "JSON integer out of range");
        assert_eq!(v.get_int64(), Ok(99));
    }
}

//! The JSON layer of `settings.json` and `banlist.json`: a port of Core's UniValue reader
//! (`UniValue::read`, `getJsonToken` in `univalue_read.cpp`, `JSONUTF8StringFilter` in
//! `univalue_utffilter.h`) and of the accessors the loaders use (`operator[]`, `getValues`,
//! `getInt`), so that exactly the documents Core accepts are accepted and read the same way.
//!
//! What follows from the port, and differs from a generic JSON library:
//! - an object keeps **every** member in file order, duplicates included; a lookup returns the
//!   **first** member with the key ([`Object::get`], Core's `findKey`);
//! - a number is kept as its source text ([`Value::Num`]); it is only converted, and range
//!   checked, by the accessor a caller asks for ([`Value::get_i64`]), as `getInt` does;
//! - strings go through Core's UTF-8 filter, which re-encodes overlong forms, joins `\u`
//!   surrogate pairs (also in UTF-8 form) and accepts code points up to `0x1FFFFF`; the result is
//!   therefore bytes ([`JsonStr`]), not always a Rust `str`;
//! - the grammar quirks of the lexer (a lone `-` at the end of input is a number token, only
//!   space, tab, CR and LF are whitespace) are kept.
//!
//! The depth limit is Core's own 512, not a bound. The reader keeps its work stack on the heap, as
//! Core's does, so a document 512 deep (or 513, which it refuses) cannot overflow the thread's
//! stack. The one difference is a bound, listed in the crate's table: the input size is capped
//! before parsing.

use std::fmt;

use super::envelope::check_size;
use super::error::{FormatError, FormatResult};

/// The size cap of `banlist.json`. Core writes at most a few thousand bans (about 150 bytes
/// each). The parsed tree is bounded by 72 heap bytes per input byte (see the "Memory of a parsed
/// JSON file" row of the crate's bounds table), so this cap is also what keeps the memory finite.
pub const MAX_JSON_FILE_SIZE: usize = 16 << 20;
/// The size cap of `settings.json`: a few dozen keys, written by hand or by the wallet RPCs.
pub const MAX_SETTINGS_FILE_SIZE: usize = 1 << 20;

/// `MAX_JSON_DEPTH` in `univalue_read.cpp`: the most containers open at once.
const MAX_JSON_DEPTH: usize = 512;

/// A JSON string as UniValue holds it: the bytes `JSONUTF8StringFilter` produced. They are valid
/// UTF-8 except for code points above `U+10FFFF`, which the filter lets through.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct JsonStr(Vec<u8>);

impl JsonStr {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The string as `str`, if it is valid UTF-8.
    pub fn to_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }
}

impl PartialEq<str> for JsonStr {
    fn eq(&self, other: &str) -> bool {
        self.0 == other.as_bytes()
    }
}

impl fmt::Debug for JsonStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&String::from_utf8_lossy(&self.0), f)
    }
}

/// A JSON object as UniValue stores it: members in file order, keys not necessarily unique.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Object {
    entries: Vec<(JsonStr, Value)>,
}

impl Object {
    /// All members in file order.
    pub fn entries(&self) -> &[(JsonStr, Value)] {
        &self.entries
    }

    pub fn into_entries(self) -> Vec<(JsonStr, Value)> {
        self.entries
    }

    /// The value of the first member named `key` (`UniValue::find_value`).
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| *k == *key).map(|(_, v)| v)
    }
}

/// A parsed JSON value (`UniValue`). The reader and the drop are iterative; the derived `Clone`,
/// `PartialEq` and `Debug` recurse once per level, up to Core's 512, so use them on a thread with
/// stack for that (a debug build needs more than the reader's own 64 KiB).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    /// The number as written (`1e400` and `18446744073709551617` stay as they are).
    Num(String),
    Str(JsonStr),
    Arr(Vec<Value>),
    Obj(Object),
}

pub(super) static NULL: Value = Value::Null;

/// Takes the last child out of an array or object.
fn pop_child(value: &mut Value) -> Option<Value> {
    match value {
        Value::Arr(items) => items.pop(),
        Value::Obj(object) => object.entries.pop().map(|(_, child)| child),
        _ => None,
    }
}

fn has_children(value: &Value) -> bool {
    match value {
        Value::Arr(items) => !items.is_empty(),
        Value::Obj(object) => !object.entries.is_empty(),
        _ => false,
    }
}

/// A tree is dropped without recursing: the derived drop glue would use stack in proportion to
/// the depth of the document (more than 64 KiB at Core's 512 in a debug build), and an overflow is
/// an abort. Children are taken out one at a time; one with children of its own goes on a heap
/// list (at most as long as the depth, so no more memory than the tree) and is unwound the same
/// way, and one without drops at once.
impl Drop for Value {
    fn drop(&mut self) {
        let mut unwinding: Vec<Value> = Vec::new();
        loop {
            let top = unwinding.last_mut().unwrap_or(&mut *self);
            match pop_child(top) {
                Some(child) if has_children(&child) => unwinding.push(child),
                Some(_) => {}
                None => {
                    if unwinding.pop().is_none() {
                        break;
                    }
                }
            }
        }
    }
}

impl Value {
    /// `UniValue::operator[](key)`: the first member named `key` of an object, else null (also
    /// for a value that is not an object).
    pub fn member(&self, key: &str) -> &Value {
        match self {
            Value::Obj(o) => o.get(key).unwrap_or(&NULL),
            _ => &NULL,
        }
    }

    /// `UniValue::getValues`: the elements of an array, or the member values of an object **in
    /// file order**. Any other value is `None` (Core throws).
    pub fn values(&self) -> Option<Vec<&Value>> {
        match self {
            Value::Arr(a) => Some(a.iter().collect()),
            Value::Obj(o) => Some(o.entries.iter().map(|(_, v)| v).collect()),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Value::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_json_str(&self) -> Option<&JsonStr> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Arr(_) => "array",
            Value::Obj(_) => "object",
        }
    }

    /// `getInt<int64_t>()`: a number whose whole text is an optionally negative integer in range.
    // Core: univalue.h UniValue::getInt (std::from_chars)
    pub fn get_i64(&self) -> Result<i64, String> {
        self.get_int(i128::from(i64::MIN), i128::from(i64::MAX)).map(|v| v as i64)
    }

    /// `getInt<int>()`.
    pub fn get_i32(&self) -> Result<i32, String> {
        self.get_int(i128::from(i32::MIN), i128::from(i32::MAX)).map(|v| v as i32)
    }

    fn get_int(&self, min: i128, max: i128) -> Result<i128, String> {
        let Value::Num(text) = self else {
            return Err(format!(
                "JSON value of type {} is not of expected type num",
                self.type_name()
            ));
        };
        let (negative, digits) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.as_str()),
        };
        let out_of_range = || "JSON integer out of range".to_owned();
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(out_of_range());
        }
        let mut magnitude: i128 = 0;
        for b in digits.bytes() {
            magnitude = magnitude
                .checked_mul(10)
                .and_then(|m| m.checked_add(i128::from(b - b'0')))
                .filter(|m| *m <= max.unsigned_abs() as i128 + 1)
                .ok_or_else(out_of_range)?;
        }
        let value = if negative {
            -magnitude
        } else {
            magnitude
        };
        (min..=max).contains(&value).then_some(value).ok_or_else(out_of_range)
    }
}

// ---------------------------------------------------------------------------------------------
// Lexer: `getJsonToken`

enum Tok {
    /// `JTOK_NONE`: only whitespace remains.
    None,
    Err,
    ObjOpen,
    ObjClose,
    ArrOpen,
    ArrClose,
    Colon,
    Comma,
    Null,
    True,
    False,
    Num(String),
    Str(JsonStr),
}

/// `JSONUTF8StringFilter`.
struct Utf8Filter {
    out: Vec<u8>,
    is_valid: bool,
    codepoint: u32,
    state: u32,
    surpair: u32,
}

impl Utf8Filter {
    fn new() -> Self {
        Utf8Filter {
            out: Vec::new(),
            is_valid: true,
            codepoint: 0,
            state: 0,
            surpair: 0,
        }
    }

    fn push_back(&mut self, ch: u8) {
        if self.state == 0 {
            if ch < 0x80 {
                self.out.push(ch);
            } else if ch < 0xc0 {
                self.is_valid = false;
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
                self.is_valid = false;
            }
        } else {
            if ch & 0xc0 != 0x80 {
                self.is_valid = false;
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

    fn append(&mut self, cp: u32) {
        if cp <= 0x7f {
            self.out.push(cp as u8);
        } else if cp <= 0x7ff {
            self.out.extend([0xC0 | (cp >> 6) as u8, 0x80 | (cp & 0x3F) as u8]);
        } else if cp <= 0xffff {
            self.out.extend([
                0xE0 | (cp >> 12) as u8,
                0x80 | ((cp >> 6) & 0x3F) as u8,
                0x80 | (cp & 0x3F) as u8,
            ]);
        } else if cp <= 0x1f_ffff {
            self.out.extend([
                0xF0 | (cp >> 18) as u8,
                0x80 | ((cp >> 12) & 0x3F) as u8,
                0x80 | ((cp >> 6) & 0x3F) as u8,
                0x80 | (cp & 0x3F) as u8,
            ]);
        }
    }
}

struct Lexer<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Lexer<'_> {
    /// The byte at `i`, or NUL past the end (the C++ reads the string's terminator there).
    fn at(&self, i: usize) -> u8 {
        self.s.get(i).copied().unwrap_or(0)
    }

    fn digits(&mut self, out: &mut String) {
        while self.pos < self.s.len() && self.s[self.pos].is_ascii_digit() {
            out.push(char::from(self.s[self.pos]));
            self.pos += 1;
        }
    }

    fn next(&mut self) -> Tok {
        while self.pos < self.s.len() && matches!(self.s[self.pos], 0x20 | 0x09 | 0x0a | 0x0d) {
            self.pos += 1;
        }
        if self.pos >= self.s.len() {
            return Tok::None;
        }
        let rest = &self.s[self.pos..];
        match rest[0] {
            b'{' => self.single(Tok::ObjOpen),
            b'}' => self.single(Tok::ObjClose),
            b'[' => self.single(Tok::ArrOpen),
            b']' => self.single(Tok::ArrClose),
            b':' => self.single(Tok::Colon),
            b',' => self.single(Tok::Comma),
            b'n' | b't' | b'f' => {
                for (word, len, tok) in [
                    (&b"null"[..], 4, Tok::Null),
                    (&b"true"[..], 4, Tok::True),
                    (&b"false"[..], 5, Tok::False),
                ] {
                    if rest.starts_with(word) {
                        self.pos += len;
                        return tok;
                    }
                }
                Tok::Err
            }
            b'-' | b'0'..=b'9' => self.number(),
            b'"' => self.string(),
            _ => Tok::Err,
        }
    }

    fn single(&mut self, tok: Tok) -> Tok {
        self.pos += 1;
        tok
    }

    fn number(&mut self) -> Tok {
        let first = self.pos;
        let first_digit = if self.at(first).is_ascii_digit() {
            first
        } else {
            first + 1
        };
        if self.at(first_digit) == b'0' && self.at(first_digit + 1).is_ascii_digit() {
            return Tok::Err;
        }
        let mut text = String::new();
        text.push(char::from(self.at(first)));
        self.pos += 1;
        if self.at(first) == b'-' && self.pos < self.s.len() && !self.s[self.pos].is_ascii_digit() {
            return Tok::Err;
        }
        self.digits(&mut text);
        if self.pos < self.s.len() && self.s[self.pos] == b'.' {
            text.push('.');
            self.pos += 1;
            if self.pos >= self.s.len() || !self.s[self.pos].is_ascii_digit() {
                return Tok::Err;
            }
            self.digits(&mut text);
        }
        if self.pos < self.s.len() && matches!(self.s[self.pos], b'e' | b'E') {
            text.push(char::from(self.s[self.pos]));
            self.pos += 1;
            if self.pos < self.s.len() && matches!(self.s[self.pos], b'-' | b'+') {
                text.push(char::from(self.s[self.pos]));
                self.pos += 1;
            }
            if self.pos >= self.s.len() || !self.s[self.pos].is_ascii_digit() {
                return Tok::Err;
            }
            self.digits(&mut text);
        }
        Tok::Num(text)
    }

    fn string(&mut self) -> Tok {
        self.pos += 1; // the opening quote
        let mut filter = Utf8Filter::new();
        loop {
            if self.pos >= self.s.len() || self.s[self.pos] < 0x20 {
                return Tok::Err;
            }
            let c = self.s[self.pos];
            if c == b'\\' {
                self.pos += 1;
                if self.pos >= self.s.len() {
                    return Tok::Err;
                }
                match self.s[self.pos] {
                    b'"' => filter.push_back(b'"'),
                    b'\\' => filter.push_back(b'\\'),
                    b'/' => filter.push_back(b'/'),
                    b'b' => filter.push_back(0x08),
                    b'f' => filter.push_back(0x0c),
                    b'n' => filter.push_back(b'\n'),
                    b'r' => filter.push_back(b'\r'),
                    b't' => filter.push_back(b'\t'),
                    b'u' => {
                        // `raw + 1 + 4 >= end`: the four digits and one more byte must exist.
                        if self.pos + 5 >= self.s.len() {
                            return Tok::Err;
                        }
                        let mut cp = 0u32;
                        for &h in &self.s[self.pos + 1..self.pos + 5] {
                            let Some(d) = char::from(h).to_digit(16) else {
                                return Tok::Err;
                            };
                            cp = 16 * cp + d;
                        }
                        filter.push_back_u(cp);
                        self.pos += 4;
                    }
                    _ => return Tok::Err,
                }
                self.pos += 1;
            } else if c == b'"' {
                self.pos += 1;
                break;
            } else {
                filter.push_back(c);
                self.pos += 1;
            }
        }
        if !filter.finalize() {
            return Tok::Err;
        }
        Tok::Str(JsonStr(filter.out))
    }
}

// ---------------------------------------------------------------------------------------------
// Reader: `UniValue::read`

/// A container being read: what `UniValue::read` keeps on its `stack` of `UniValue*`.
enum Frame {
    Arr(Vec<Value>),
    /// The members so far, and the key whose value is being read.
    Obj(Vec<(JsonStr, Value)>, JsonStr),
}

/// `UniValue::read`'s state machine with its work stack on the heap, as in Core
/// (`std::vector<UniValue*> stack`): the depth of the document costs heap, never thread stack, so
/// a document 512 deep is read (or refused) on any thread. Returns the one value `lex` starts
/// with; what follows it is the caller's to check.
// Core: univalue_read.cpp UniValue::read
fn read_value(lex: &mut Lexer<'_>) -> Option<Value> {
    let mut stack: Vec<Frame> = Vec::new();
    // The token that starts the next value.
    let mut tok = lex.next();
    loop {
        let mut done = match tok {
            Tok::Null => Value::Null,
            Tok::True => Value::Bool(true),
            Tok::False => Value::Bool(false),
            Tok::Num(text) => Value::Num(text),
            Tok::Str(s) => Value::Str(s),
            Tok::ObjOpen => {
                if stack.len() + 1 > MAX_JSON_DEPTH {
                    return None;
                }
                match lex.next() {
                    Tok::ObjClose => Value::Obj(Object::default()),
                    Tok::Str(key) => {
                        if !matches!(lex.next(), Tok::Colon) {
                            return None;
                        }
                        stack.push(Frame::Obj(Vec::new(), key));
                        tok = lex.next();
                        continue;
                    }
                    _ => return None,
                }
            }
            Tok::ArrOpen => {
                if stack.len() + 1 > MAX_JSON_DEPTH {
                    return None;
                }
                let first = lex.next();
                if matches!(first, Tok::ArrClose) {
                    Value::Arr(Vec::new())
                } else {
                    stack.push(Frame::Arr(Vec::new()));
                    tok = first;
                    continue;
                }
            }
            Tok::None | Tok::Err | Tok::ObjClose | Tok::ArrClose | Tok::Colon | Tok::Comma => {
                return None;
            }
        };
        // `done` is a complete value: hand it to its container, and close containers while the
        // closing bracket follows.
        loop {
            match stack.last_mut() {
                None => return Some(done),
                Some(Frame::Arr(items)) => {
                    items.push(done);
                    match lex.next() {
                        Tok::Comma => {
                            tok = lex.next();
                            break;
                        }
                        Tok::ArrClose => {}
                        _ => return None,
                    }
                }
                Some(Frame::Obj(entries, key)) => {
                    entries.push((std::mem::take(key), done));
                    match lex.next() {
                        Tok::Comma => {
                            let Tok::Str(next) = lex.next() else {
                                return None;
                            };
                            if !matches!(lex.next(), Tok::Colon) {
                                return None;
                            }
                            *key = next;
                            tok = lex.next();
                            break;
                        }
                        Tok::ObjClose => {}
                        _ => return None,
                    }
                }
            }
            done = match stack.pop() {
                Some(Frame::Arr(items)) => Value::Arr(items),
                Some(Frame::Obj(entries, _)) => Value::Obj(Object {
                    entries,
                }),
                None => return None,
            };
        }
    }
}

fn err(what: &'static str, message: impl fmt::Display) -> FormatError {
    FormatError::Json {
        what,
        message: message.to_string(),
    }
}

/// `UniValue::read` on `bytes`: one JSON value with nothing but whitespace after it.
pub(crate) fn parse_value(bytes: &[u8], what: &'static str, limit: usize) -> FormatResult<Value> {
    check_size(bytes.len(), limit, what)?;
    let mut lex = Lexer {
        s: bytes,
        pos: 0,
    };
    let value = read_value(&mut lex).ok_or_else(|| err(what, "does not contain valid JSON"))?;
    if !matches!(lex.next(), Tok::None) {
        return Err(err(what, "does not contain valid JSON"));
    }
    Ok(value)
}

/// A settings-style file (`util::ReadSettings`): one JSON object whose **top-level** keys are
/// unique. Nested objects keep their duplicates, as UniValue does.
// Core: util/settings.cpp ReadSettings
pub(crate) fn parse_settings_object(
    bytes: &[u8],
    what: &'static str,
    limit: usize,
) -> FormatResult<Object> {
    let mut value = parse_value(bytes, what, limit)?;
    // `Value` has a `Drop`, so the object is taken out rather than moved out.
    let Value::Obj(object) = &mut value else {
        return Err(err(what, "contains a non-object value"));
    };
    let object = std::mem::take(object);
    let mut seen = std::collections::HashSet::new();
    for (key, _) in &object.entries {
        if !seen.insert(key) {
            return Err(err(what, format_args!("contains a duplicate key {key:?}")));
        }
    }
    Ok(object)
}

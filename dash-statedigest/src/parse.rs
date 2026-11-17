//! The strict parser (spec §3): [`parse_line`] and [`Lines`].
//!
//! The parser accepts exactly the canonical lines. Anything else is an error carrying the byte
//! offset and the reason. As a last guard, a parsed record is re-encoded and compared with the
//! input, so a gap in the strictness above would surface as [`ParseErrorKind::NotCanonical`]
//! rather than as bytes that differ from what a conforming writer produces.

use core::fmt;

use crate::fields::{self, Class, Fields, Kind};
use crate::record::{
    Attempt, Chain, FileKind, Header, Ident, MAX_DEPTH, Origin, Outcome, Record, SPEC_VERSION,
    Sparse, Writer, is_number_text,
};
use crate::typed::Query;
use crate::value::Value;

/// Why a line was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// The line is `\n` alone (no blank lines).
    BlankLine,
    /// The input ends where more is required.
    UnexpectedEnd,
    /// The final line has no `\n`.
    MissingTerminator,
    /// A space or tab outside a string (the grammar has no whitespace).
    Whitespace,
    /// A `\r`.
    CarriageReturn,
    /// A UTF-8 byte order mark at the start.
    Bom,
    /// A byte that the grammar does not allow here.
    UnexpectedByte {
        found: u8,
        expected: &'static str,
    },
    /// A string is not closed before the end of the line.
    UnterminatedString,
    /// A byte outside 0x20-0x7e inside a string, not written as `\u00xx`.
    RawByteInString(u8),
    /// An escape that is not canonical (the only escapes are `\"`, `\\` and `\u00xx` for bytes
    /// that cannot appear verbatim, with lowercase hex).
    BadEscape(&'static str),
    /// An integer outside the grammar (`-?(0|[1-9][0-9]*)`, `-0` invalid).
    BadInteger(&'static str),
    /// An integer outside the signed 64-bit range.
    IntegerOutOfRange,
    /// A fraction or exponent where only integers are allowed (anywhere outside `res`).
    FloatNotAllowed,
    /// Number text in `res` outside the number grammar.
    BadNumber,
    /// A key repeated.
    DuplicateKey,
    /// A key this record kind does not define.
    UnknownKey,
    /// A known key in the wrong place: keys follow spec order, and the top-level keys of a record
    /// are all required, so a skipped required key shows up as the next key being out of order.
    KeyOutOfOrder,
    /// The object ends before the required key.
    MissingKey(&'static str),
    /// Keys of a payload object (in `res` or `args`) are not strictly ascending bytewise.
    UnsortedKeys,
    /// A value of the wrong shape for its key.
    WrongShape {
        key: &'static str,
        why: &'static str,
    },
    /// A `"k"` that is not one of `H`, `C`, `R`, `D`, `A`, `S`.
    UnknownKind,
    /// An `H` whose `v` is not 1.
    UnsupportedVersion,
    /// Arrays and objects nested deeper than [`MAX_DEPTH`].
    TooDeep,
    /// Bytes after the line's `\n`, or after `}` where `\n` belongs.
    TrailingBytes,
    /// The line parsed, but re-encoding it gives different bytes (offset of the first
    /// difference). The strict rules above make this unreachable; it is a safety net.
    NotCanonical,
}

impl fmt::Display for ParseErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ParseErrorKind::*;
        match self {
            BlankLine => f.write_str("blank line"),
            UnexpectedEnd => f.write_str("unexpected end of line"),
            MissingTerminator => f.write_str("line is not terminated by \\n"),
            Whitespace => f.write_str("whitespace outside a string"),
            CarriageReturn => f.write_str("carriage return"),
            Bom => f.write_str("byte order mark"),
            UnexpectedByte {
                found,
                expected,
            } => {
                write!(f, "unexpected byte 0x{found:02x}, expected {expected}")
            }
            UnterminatedString => f.write_str("unterminated string"),
            RawByteInString(b) => write!(f, "byte 0x{b:02x} must be written as \\u00{b:02x}"),
            BadEscape(why) => write!(f, "bad escape: {why}"),
            BadInteger(why) => write!(f, "bad integer: {why}"),
            IntegerOutOfRange => f.write_str("integer outside the signed 64-bit range"),
            FloatNotAllowed => f.write_str("fraction or exponent outside an S payload"),
            BadNumber => f.write_str("number text outside the number grammar"),
            DuplicateKey => f.write_str("duplicate key"),
            UnknownKey => f.write_str("unknown key"),
            KeyOutOfOrder => {
                f.write_str("key out of order (or an earlier required key is missing)")
            }
            MissingKey(key) => write!(f, "missing key `{key}`"),
            UnsortedKeys => f.write_str("payload object keys are not strictly ascending"),
            WrongShape {
                key,
                why,
            } => write!(f, "value of `{key}`: {why}"),
            UnknownKind => f.write_str("unknown record kind"),
            UnsupportedVersion => {
                write!(f, "unsupported spec version (this crate reads v{SPEC_VERSION})")
            }
            TooDeep => write!(f, "nesting deeper than {MAX_DEPTH}"),
            TrailingBytes => f.write_str("trailing bytes after the record"),
            NotCanonical => f.write_str("record does not re-encode to the same bytes"),
        }
    }
}

/// A refused line: the reason and the byte offset it was found at. Offsets are relative to the
/// slice given to [`parse_line`], and to the whole buffer when the error comes from [`Lines`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub offset: usize,
    pub kind: ParseErrorKind,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "byte {}: {}", self.offset, self.kind)
    }
}

impl core::error::Error for ParseError {}

type Res<T> = Result<T, ParseError>;

/// How numbers are read: integers everywhere but the `res` payload of an `S` record, where every
/// number is kept as Core's text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NumMode {
    Int,
    Num,
}

const HEADER_KEYS: &[&str] = &["k", "v", "file", "src", "ver", "chain", "run"];
const OUTCOME_KEYS: &[&str] = &["k", "h", "b", "org", "s", "o", "x"];
const ATTEMPT_KEYS: &[&str] = &["k", "h", "b", "org", "t", "q", "x"];
const SPARSE_KEYS: &[&str] = &["k", "h", "b", "rpc", "args", "res"];

struct Parser<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn fail<T>(&self, offset: usize, kind: ParseErrorKind) -> Res<T> {
        Err(ParseError {
            offset,
            kind,
        })
    }

    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    /// The error for the byte at the cursor not being what the grammar needs.
    fn unexpected<T>(&self, expected: &'static str) -> Res<T> {
        let kind = match self.peek() {
            None | Some(b'\n') => ParseErrorKind::UnexpectedEnd,
            Some(b' ' | b'\t') => ParseErrorKind::Whitespace,
            Some(b'\r') => ParseErrorKind::CarriageReturn,
            Some(0xef) if self.pos == 0 && self.buf.starts_with(&[0xef, 0xbb, 0xbf]) => {
                ParseErrorKind::Bom
            }
            Some(found) => ParseErrorKind::UnexpectedByte {
                found,
                expected,
            },
        };
        self.fail(self.pos, kind)
    }

    fn expect(&mut self, byte: u8, what: &'static str) -> Res<()> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            self.unexpected(what)
        }
    }

    fn literal(&mut self, text: &'static [u8]) -> Res<()> {
        // Advance over the matching prefix, so a mismatch is reported where it is.
        for &expected in text {
            if self.peek() != Some(expected) {
                return self.unexpected("a value");
            }
            self.pos += 1;
        }
        Ok(())
    }

    fn string(&mut self) -> Res<Vec<u8>> {
        self.expect(b'"', "a string")?;
        let mut out = Vec::new();
        loop {
            let at = self.pos;
            let Some(b) = self.peek() else {
                return self.fail(at, ParseErrorKind::UnterminatedString);
            };
            self.pos += 1;
            match b {
                b'"' => return Ok(out),
                b'\n' => return self.fail(at, ParseErrorKind::UnterminatedString),
                0x20..=0x7e if b != b'\\' => out.push(b),
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return self.fail(at, ParseErrorKind::UnterminatedString);
                    };
                    self.pos += 1;
                    match e {
                        b'"' | b'\\' => out.push(e),
                        b'u' => out.push(self.unicode_escape(at)?),
                        _ => {
                            return self.fail(
                                at,
                                ParseErrorKind::BadEscape("only \\\", \\\\ and \\u00xx exist"),
                            );
                        }
                    }
                }
                _ => return self.fail(at, ParseErrorKind::RawByteInString(b)),
            }
        }
    }

    /// The four digits after `\u`: `00` and two lowercase hex digits, for a byte that cannot
    /// appear verbatim.
    fn unicode_escape(&mut self, at: usize) -> Res<u8> {
        let digits = self.buf.get(self.pos..self.pos + 4);
        let value = match digits {
            Some([b'0', b'0', hi, lo]) => {
                match (crate::hex::nibble(*hi), crate::hex::nibble(*lo)) {
                    (Some(hi), Some(lo)) => hi << 4 | lo,
                    _ => {
                        return self.fail(
                            at,
                            ParseErrorKind::BadEscape(
                                "\\u00 must be followed by two lowercase hex digits",
                            ),
                        );
                    }
                }
            }
            _ => {
                return self.fail(
                    at,
                    ParseErrorKind::BadEscape("\\u must be \\u00 and two lowercase hex digits"),
                );
            }
        };
        if (0x20..=0x7e).contains(&value) {
            return self.fail(at, ParseErrorKind::BadEscape("this byte must be written verbatim"));
        }
        self.pos += 4;
        Ok(value)
    }

    fn int(&mut self) -> Res<i64> {
        let start = self.pos;
        let neg = self.peek() == Some(b'-');
        if neg {
            self.pos += 1;
        }
        let digits = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == digits {
            return self.fail(self.pos, ParseErrorKind::BadInteger("a digit is required"));
        }
        if self.buf[digits] == b'0' && self.pos - digits > 1 {
            return self.fail(digits, ParseErrorKind::BadInteger("leading zero"));
        }
        if matches!(self.peek(), Some(b'.' | b'e' | b'E')) {
            return self.fail(self.pos, ParseErrorKind::FloatNotAllowed);
        }
        let mut magnitude: u64 = 0;
        for &d in &self.buf[digits..self.pos] {
            magnitude =
                match magnitude.checked_mul(10).and_then(|m| m.checked_add(u64::from(d - b'0'))) {
                    Some(m) => m,
                    None => return self.fail(start, ParseErrorKind::IntegerOutOfRange),
                };
        }
        if neg && magnitude == 0 {
            return self.fail(start, ParseErrorKind::BadInteger("-0"));
        }
        let signed = if neg {
            -i128::from(magnitude)
        } else {
            i128::from(magnitude)
        };
        match i64::try_from(signed) {
            Ok(n) => Ok(n),
            Err(_) => self.fail(start, ParseErrorKind::IntegerOutOfRange),
        }
    }

    fn number_text(&mut self) -> Res<String> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|b| matches!(b, b'0'..=b'9' | b'+' | b'-' | b'.' | b'e' | b'E'))
        {
            self.pos += 1;
        }
        let text = core::str::from_utf8(&self.buf[start..self.pos]).unwrap_or("");
        if is_number_text(text) {
            Ok(text.to_owned())
        } else {
            self.fail(start, ParseErrorKind::BadNumber)
        }
    }

    fn value(&mut self, nums: NumMode, depth: usize) -> Res<Value> {
        if depth > MAX_DEPTH {
            return self.fail(self.pos, ParseErrorKind::TooDeep);
        }
        match self.peek() {
            Some(b'n') => self.literal(b"null").map(|()| Value::Null),
            Some(b't') => self.literal(b"true").map(|()| Value::Bool(true)),
            Some(b'f') => self.literal(b"false").map(|()| Value::Bool(false)),
            Some(b'"') => self.string().map(Value::Str),
            Some(b'-' | b'0'..=b'9') => match nums {
                NumMode::Int => self.int().map(Value::Int),
                NumMode::Num => self.number_text().map(Value::Num),
            },
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                if self.peek() == Some(b']') {
                    self.pos += 1;
                    return Ok(Value::Arr(items));
                }
                loop {
                    items.push(self.value(nums, depth + 1)?);
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(Value::Arr(items));
                        }
                        _ => return self.unexpected("`,` or `]`"),
                    }
                }
            }
            Some(b'{') => {
                self.pos += 1;
                let mut members: Vec<(Vec<u8>, Value)> = Vec::new();
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                    return Ok(Value::Obj(members));
                }
                loop {
                    let at = self.pos;
                    let key = self.string()?;
                    if let Some((prev, _)) = members.last() {
                        if *prev == key {
                            return self.fail(at, ParseErrorKind::DuplicateKey);
                        }
                        if *prev > key {
                            return self.fail(at, ParseErrorKind::UnsortedKeys);
                        }
                    }
                    self.expect(b':', "`:`")?;
                    members.push((key, self.value(nums, depth + 1)?));
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(Value::Obj(members));
                        }
                        _ => return self.unexpected("`,` or `}`"),
                    }
                }
            }
            _ => self.unexpected("a value"),
        }
    }

    /// Reads the next top-level key, which must be `order[idx]`, and the `:` after it.
    fn key(&mut self, order: &[&'static str], idx: usize) -> Res<()> {
        if idx > 0 {
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => return self.fail(self.pos, ParseErrorKind::MissingKey(order[idx])),
                _ => return self.unexpected("`,`"),
            }
        }
        let at = self.pos;
        let key = self.string()?;
        if key == order[idx].as_bytes() {
            return self.expect(b':', "`:`");
        }
        let kind = match order.iter().position(|k| k.as_bytes() == key) {
            Some(j) if j < idx => ParseErrorKind::DuplicateKey,
            Some(_) => ParseErrorKind::KeyOutOfOrder,
            None => ParseErrorKind::UnknownKey,
        };
        self.fail(at, kind)
    }

    /// The `}` after the last top-level key.
    fn end_object(&mut self, order: &[&'static str]) -> Res<()> {
        match self.peek() {
            Some(b'}') => {
                self.pos += 1;
                Ok(())
            }
            Some(b',') => {
                self.pos += 1;
                let at = self.pos;
                let key = self.string()?;
                let kind = if order.iter().any(|k| k.as_bytes() == key) {
                    ParseErrorKind::DuplicateKey
                } else {
                    ParseErrorKind::UnknownKey
                };
                self.fail(at, kind)
            }
            _ => self.unexpected("`}`"),
        }
    }

    /// Reads a value and converts it; a refusal is reported at the value's first byte.
    fn typed<T>(
        &mut self,
        key: &'static str,
        nums: NumMode,
        convert: impl FnOnce(&Value) -> Result<T, &'static str>,
    ) -> Res<T> {
        let at = self.pos;
        let value = self.value(nums, 0)?;
        convert(&value).or_else(|why| {
            self.fail(
                at,
                ParseErrorKind::WrongShape {
                    key,
                    why,
                },
            )
        })
    }

    fn ident(&mut self, order: &[&'static str]) -> Res<Ident> {
        self.key(order, 1)?;
        let h = self.typed("h", NumMode::Int, |v| v.as_int().ok_or("expected an integer"))?;
        self.key(order, 2)?;
        let b = self.typed("b", NumMode::Int, |v| {
            v.as_hash().ok_or("expected a hash (64 lowercase hex digits)")
        })?;
        self.key(order, 3)?;
        let org = self.typed("org", NumMode::Int, |v| {
            v.as_bytes()
                .and_then(Origin::from_bytes)
                .ok_or("expected tip, proposal, verifydb or replay")
        })?;
        Ok(Ident {
            h,
            b,
            org,
        })
    }

    fn fields(&mut self, kind: Kind, class: Class) -> Res<Fields> {
        let group = fields::group(kind, class);
        let mut out = Fields::new(kind, class);
        self.expect(b'{', "an object")?;
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(out);
        }
        let mut last: Option<usize> = None;
        let mut seen = 0u64;
        loop {
            let at = self.pos;
            let key = self.string()?;
            let Some(idx) = group.iter().position(|f| f.key.as_bytes() == key) else {
                return self.fail(at, ParseErrorKind::UnknownKey);
            };
            if seen >> idx & 1 == 1 {
                return self.fail(at, ParseErrorKind::DuplicateKey);
            }
            if last.is_some_and(|l| idx < l) {
                return self.fail(at, ParseErrorKind::KeyOutOfOrder);
            }
            seen |= 1 << idx;
            last = Some(idx);
            self.expect(b':', "`:`")?;
            let at = self.pos;
            let value = self.value(NumMode::Int, 0)?;
            if let Err(why) = group[idx].shape.check(&value) {
                return self.fail(
                    at,
                    ParseErrorKind::WrongShape {
                        key: group[idx].key,
                        why,
                    },
                );
            }
            out.put(idx, value);
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(out);
                }
                _ => return self.unexpected("`,` or `}`"),
            }
        }
    }

    fn record(&mut self) -> Res<Record> {
        self.expect(b'{', "`{`")?;
        let at = self.pos;
        let first = self.string()?;
        if first != b"k" {
            return self.fail(at, ParseErrorKind::MissingKey("k"));
        }
        self.expect(b':', "`:`")?;
        let at = self.pos;
        let letter = self.string()?;
        let Some(kind) = Kind::from_letter(&letter) else {
            return self.fail(at, ParseErrorKind::UnknownKind);
        };
        match kind {
            Kind::Header => {
                self.key(HEADER_KEYS, 1)?;
                let at = self.pos;
                let v =
                    self.typed("v", NumMode::Int, |v| v.as_int().ok_or("expected an integer"))?;
                if v != SPEC_VERSION {
                    return self.fail(at, ParseErrorKind::UnsupportedVersion);
                }
                self.key(HEADER_KEYS, 2)?;
                let file = self.typed("file", NumMode::Int, |v| {
                    v.as_bytes()
                        .and_then(FileKind::from_bytes)
                        .ok_or("expected digest, decisions, golden or memory")
                })?;
                self.key(HEADER_KEYS, 3)?;
                let src = self.typed("src", NumMode::Int, |v| {
                    v.as_bytes()
                        .and_then(Writer::from_bytes)
                        .ok_or("expected dashd, rdashd or extract")
                })?;
                self.key(HEADER_KEYS, 4)?;
                let ver = self.typed("ver", NumMode::Int, |v| {
                    v.as_bytes().map(<[u8]>::to_vec).ok_or("expected a string")
                })?;
                self.key(HEADER_KEYS, 5)?;
                let chain = self.typed("chain", NumMode::Int, |v| {
                    v.as_bytes()
                        .and_then(Chain::from_bytes)
                        .ok_or("expected main, test, regtest or devnet")
                })?;
                self.key(HEADER_KEYS, 6)?;
                let run = self.typed("run", NumMode::Int, |v| {
                    v.as_bytes().map(<[u8]>::to_vec).ok_or("expected a string")
                })?;
                self.end_object(HEADER_KEYS)?;
                Ok(Record::Header(Header {
                    file,
                    src,
                    ver,
                    chain,
                    run,
                }))
            }
            Kind::Connect | Kind::Reject | Kind::Disconnect => {
                let id = self.ident(OUTCOME_KEYS)?;
                self.key(OUTCOME_KEYS, 4)?;
                let s = self.fields(kind, Class::Semantic)?;
                self.key(OUTCOME_KEYS, 5)?;
                let o = self.fields(kind, Class::Observation)?;
                self.key(OUTCOME_KEYS, 6)?;
                let x = self.fields(kind, Class::Schedule)?;
                self.end_object(OUTCOME_KEYS)?;
                Ok(Record::Outcome(Outcome::from_parts(kind, id, s, o, x)))
            }
            Kind::Attempt => {
                let id = self.ident(ATTEMPT_KEYS)?;
                self.key(ATTEMPT_KEYS, 4)?;
                let t =
                    self.typed("t", NumMode::Int, |v| v.as_int().ok_or("expected an integer"))?;
                self.key(ATTEMPT_KEYS, 5)?;
                let q =
                    self.typed("q", NumMode::Int, |v| Query::list_from_value(v).map_err(|e| e.0))?;
                self.key(ATTEMPT_KEYS, 6)?;
                let x = self.fields(Kind::Attempt, Class::Schedule)?;
                self.end_object(ATTEMPT_KEYS)?;
                Ok(Record::Attempt(Attempt::from_parts(id, t, q, x)))
            }
            Kind::Sparse => {
                self.key(SPARSE_KEYS, 1)?;
                let h =
                    self.typed("h", NumMode::Int, |v| v.as_int().ok_or("expected an integer"))?;
                self.key(SPARSE_KEYS, 2)?;
                let b = self.typed("b", NumMode::Int, |v| {
                    v.as_hash().ok_or("expected a hash (64 lowercase hex digits)")
                })?;
                self.key(SPARSE_KEYS, 3)?;
                let rpc = self.typed("rpc", NumMode::Int, |v| {
                    v.as_bytes().map(<[u8]>::to_vec).ok_or("expected a string")
                })?;
                self.key(SPARSE_KEYS, 4)?;
                let args = self.typed("args", NumMode::Int, |v| match v {
                    Value::Arr(items) => Ok(items.clone()),
                    _ => Err("expected an array"),
                })?;
                self.key(SPARSE_KEYS, 5)?;
                let res = self.value(NumMode::Num, 0)?;
                self.end_object(SPARSE_KEYS)?;
                Ok(Record::Sparse(Sparse::from_parts(h, b, rpc, args, res)))
            }
        }
    }

    /// The `\n` that ends the line, and nothing after it.
    fn terminator(&mut self) -> Res<()> {
        match self.peek() {
            Some(b'\n') => {
                self.pos += 1;
                if self.pos == self.buf.len() {
                    Ok(())
                } else {
                    self.fail(self.pos, ParseErrorKind::TrailingBytes)
                }
            }
            None => self.fail(self.pos, ParseErrorKind::MissingTerminator),
            Some(b'\r') => self.fail(self.pos, ParseErrorKind::CarriageReturn),
            Some(b' ' | b'\t') => self.fail(self.pos, ParseErrorKind::Whitespace),
            Some(_) => self.fail(self.pos, ParseErrorKind::TrailingBytes),
        }
    }
}

/// Parses one canonical line: a JSON object followed by exactly one `\n`, with nothing after it.
/// This is the exact inverse of [`Record::encode`].
///
/// Rejected: whitespace, a BOM, `\r`, any escape but `\"`, `\\` and lowercase `\u00xx` for bytes
/// that cannot appear verbatim, leading zeros, `-0`, integers outside `i64`, numbers with a
/// fraction or exponent outside an `S` record's `res`, duplicate keys, keys the kind does not
/// define, keys out of spec order, values of the wrong shape, and trailing bytes. Keys inside the
/// `s`, `o` and `x` sub-objects may be absent (absent is not `null`); every other key of a record
/// is required.
pub fn parse_line(line: &[u8]) -> Result<Record, ParseError> {
    if line == b"\n" {
        return Err(ParseError {
            offset: 0,
            kind: ParseErrorKind::BlankLine,
        });
    }
    let mut parser = Parser {
        buf: line,
        pos: 0,
    };
    let record = parser.record()?;
    parser.terminator()?;
    let mut again = Vec::with_capacity(line.len());
    record.encode(&mut again);
    if again != line {
        let offset = again.iter().zip(line).take_while(|(a, b)| a == b).count();
        return Err(ParseError {
            offset,
            kind: ParseErrorKind::NotCanonical,
        });
    }
    Ok(record)
}

/// Iterates the lines of a buffer: `(line_no, parse result)` with `line_no` counting from 1.
///
/// The buffer must end with `\n` and have no blank line; a final fragment without `\n` yields
/// [`ParseErrorKind::MissingTerminator`], a blank line [`ParseErrorKind::BlankLine`]. Error
/// offsets are positions in the whole buffer. A refused line does not stop the iteration.
#[derive(Clone, Debug)]
pub struct Lines<'a> {
    buf: &'a [u8],
    pos: usize,
    line_no: usize,
}

impl<'a> Lines<'a> {
    pub fn new(buf: &'a [u8]) -> Lines<'a> {
        Lines {
            buf,
            pos: 0,
            line_no: 0,
        }
    }
}

impl Iterator for Lines<'_> {
    type Item = (usize, Result<Record, ParseError>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let start = self.pos;
        let rest = &self.buf[start..];
        self.line_no += 1;
        let Some(newline) = rest.iter().position(|&b| b == b'\n') else {
            self.pos = self.buf.len();
            let err = ParseError {
                offset: self.buf.len(),
                kind: ParseErrorKind::MissingTerminator,
            };
            return Some((self.line_no, Err(err)));
        };
        let line = &rest[..=newline];
        self.pos = start + line.len();
        let result = parse_line(line).map_err(|mut e| {
            e.offset += start;
            e
        });
        Some((self.line_no, result))
    }
}

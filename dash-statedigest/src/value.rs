//! The JSON value model of the canonical subset (spec §3) and its writer.

use crate::hex::{Hash256, write_hex};

/// A value of the canonical JSON subset.
///
/// Strings are **byte strings** (spec §3): the escaping is byte-oriented, so a string need not be
/// valid UTF-8 and is stored as `Vec<u8>`. Object keys are byte strings for the same reason (the
/// keys of an RPC result inside an `S` payload are not restricted to `[a-z0-9_]`). The spec's
/// §12 sketch says `Str(String)`; a `String` could not hold every byte string the grammar
/// accepts, so the lossless form is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// An integer in the signed 64-bit range.
    Int(i64),
    /// A number kept as Core's RPC text. Legal only inside the `res` payload of an `S` record,
    /// where *every* number is a `Num`, never an `Int`.
    Num(String),
    /// A byte string.
    Str(Vec<u8>),
    /// An array.
    Arr(Vec<Value>),
    /// An object with its keys in order. In the `res` and `args` of an `S` record the keys are
    /// strictly ascending bytewise.
    Obj(Vec<(Vec<u8>, Value)>),
}

impl Value {
    /// A string value from text or bytes.
    pub fn string(s: impl AsRef<[u8]>) -> Value {
        Value::Str(s.as_ref().to_vec())
    }

    /// A string value holding the 64-digit lowercase hex of `hash`.
    pub fn hash(hash: &Hash256) -> Value {
        Value::Str(hash.to_hex().into_bytes())
    }

    /// A string value holding `bytes` as lowercase hex.
    pub fn hex(bytes: &[u8]) -> Value {
        let mut out = Vec::with_capacity(bytes.len() * 2);
        write_hex(&mut out, bytes);
        Value::Str(out)
    }

    /// An object value from `(key, value)` pairs, in the given order.
    pub fn obj<K: AsRef<[u8]>>(pairs: impl IntoIterator<Item = (K, Value)>) -> Value {
        Value::Obj(pairs.into_iter().map(|(k, v)| (k.as_ref().to_vec(), v)).collect())
    }

    /// Whether this is `null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The boolean, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The integer, if this is an `Int`.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// The verbatim number text, if this is a `Num`.
    pub fn as_num(&self) -> Option<&str> {
        match self {
            Value::Num(n) => Some(n),
            _ => None,
        }
    }

    /// The string's bytes, if this is a string.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The string as text, if this is a string holding valid UTF-8 (every ASCII string is).
    pub fn as_str(&self) -> Option<&str> {
        self.as_bytes().and_then(|b| core::str::from_utf8(b).ok())
    }

    /// The elements, if this is an array.
    pub fn as_arr(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }

    /// The members in order, if this is an object.
    pub fn as_obj(&self) -> Option<&[(Vec<u8>, Value)]> {
        match self {
            Value::Obj(o) => Some(o),
            _ => None,
        }
    }

    /// The member `key` of an object (the first, in the canonical form the only, one).
    pub fn get(&self, key: &[u8]) -> Option<&Value> {
        self.as_obj()?.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The hash, if this is a string of 64 lowercase hex digits.
    pub fn as_hash(&self) -> Option<Hash256> {
        Hash256::from_hex(self.as_bytes()?)
    }

    /// Appends the canonical encoding (spec §3): no whitespace, integers in the shortest form,
    /// `Num` text verbatim, strings byte-escaped. The caller upholds the invariants the model
    /// documents (`Num` text in the number grammar, sorted payload keys); records built through
    /// this crate's constructors and parsed records always do.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Value::Null => out.extend_from_slice(b"null"),
            Value::Bool(true) => out.extend_from_slice(b"true"),
            Value::Bool(false) => out.extend_from_slice(b"false"),
            Value::Int(n) => write_int(out, *n),
            Value::Num(text) => out.extend_from_slice(text.as_bytes()),
            Value::Str(s) => write_string(out, s),
            Value::Arr(items) => {
                out.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    item.encode(out);
                }
                out.push(b']');
            }
            Value::Obj(members) => {
                out.push(b'{');
                for (i, (key, value)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    write_string(out, key);
                    out.push(b':');
                    value.encode(out);
                }
                out.push(b'}');
            }
        }
    }

    /// The canonical encoding as a new buffer (no line terminator).
    pub fn to_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Int(n)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::string(s)
    }
}

impl From<&Hash256> for Value {
    fn from(h: &Hash256) -> Self {
        Value::hash(h)
    }
}

/// Appends `n` in the grammar's integer form.
pub(crate) fn write_int(out: &mut Vec<u8>, n: i64) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut rest = n.unsigned_abs();
    loop {
        at -= 1;
        digits[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if n < 0 {
        out.push(b'-');
    }
    out.extend_from_slice(&digits[at..]);
}

/// Appends `bytes` as a canonical string: bytes 0x20-0x7e except `"` and `\` verbatim, `\"`,
/// `\\`, and `\u00xx` (lowercase) for every other byte.
pub(crate) fn write_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'"');
    for &b in bytes {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x20..=0x7e => out.push(b),
            _ => {
                out.extend_from_slice(b"\\u00");
                write_hex(out, &[b]);
            }
        }
    }
    out.push(b'"');
}

//! The field table (spec §5) and the ordered field maps built from it.
//!
//! [`FIELDS`] is the one place that says which keys exist, in which order, in which class and of
//! which shape. The parser validates against it; a comparator iterates it for keys and classes.

use core::fmt;

use crate::hex::Hash256;
use crate::typed::Effect;
use crate::value::Value;

/// A record kind (spec §4). The letter is the `"k"` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// `H`: file header.
    Header,
    /// `C`: a successful connect.
    Connect,
    /// `R`: an attempt that did not produce a `C`.
    Reject,
    /// `D`: a disconnect.
    Disconnect,
    /// `A`: decisions.
    Attempt,
    /// `S`: sparse golden.
    Sparse,
}

impl Kind {
    /// The `"k"` letter.
    pub const fn letter(self) -> u8 {
        match self {
            Kind::Header => b'H',
            Kind::Connect => b'C',
            Kind::Reject => b'R',
            Kind::Disconnect => b'D',
            Kind::Attempt => b'A',
            Kind::Sparse => b'S',
        }
    }

    /// The kind for a `"k"` letter.
    pub fn from_letter(text: &[u8]) -> Option<Kind> {
        Some(match text {
            b"H" => Kind::Header,
            b"C" => Kind::Connect,
            b"R" => Kind::Reject,
            b"D" => Kind::Disconnect,
            b"A" => Kind::Attempt,
            b"S" => Kind::Sparse,
            _ => return None,
        })
    }
}

/// A field's comparison class (A1 §4.6, spec §5): the sub-object it lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// `s`: equal on any two nodes on the same chain.
    Semantic,
    /// `o`: equal only for the same sequence of operations.
    Observation,
    /// `x`: never compared.
    Schedule,
}

impl Class {
    /// The key of the sub-object that holds this class: `s`, `o` or `x`.
    pub const fn key(self) -> &'static str {
        match self {
            Class::Semantic => "s",
            Class::Observation => "o",
            Class::Schedule => "x",
        }
    }
}

/// Who writes a field (spec §5): the Core patch (and rdashd), or the golden extractor (and
/// rdashd), never the patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldSource {
    Patch,
    Golden,
}

/// `R.s.res` values.
pub const REJECT_RESULTS: &[&str] = &["ok", "invalid", "error"];
/// `R.s.cls` values (`BlockValidationResult` names, spec §4.3).
pub const REJECT_CLASSES: &[&str] = &[
    "result_unset",
    "consensus",
    "recent_consensus_change",
    "cached_invalid",
    "invalid_header",
    "mutated",
    "missing_prev",
    "invalid_prev",
    "time_future",
    "checkpoint",
    "chainlock",
];
/// `D.s.res` values (`DisconnectResult`, spec §4.4).
pub const DISCONNECT_RESULTS: &[&str] = &["ok", "unclean", "failed"];
/// The deployment states of `C.s.dep` (spec §5.2).
pub const DEPLOYMENT_STATES: &[&str] = &["defined", "started", "locked_in", "active", "failed"];

/// What a field's value looks like (spec §5, §4.3, §4.4). `null` is governed by
/// [`Shape::nullable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// An integer.
    Int,
    /// An integer in `0..=u32::MAX`.
    U32,
    /// A boolean.
    Bool,
    /// Any string.
    Str,
    /// A `hash`: 64 lowercase hex digits.
    Hash,
    /// `qc`: `[[type, quorumHash, commitmentHash], …]`, strictly ascending by type, then by the
    /// quorum hash's hex.
    Qc,
    /// `cp`: `[locked, currentLimit, latelyUnlocked, indexesHash]`.
    Cp,
    /// `ehf`: `[[bit, height], …]`, strictly ascending by bit.
    Ehf,
    /// `dep`: `[[name, state, since], …]`, `state` one of [`DEPLOYMENT_STATES`].
    Dep,
    /// An array of exactly this many integers (`bi`, `stats`).
    Ints(usize),
    /// `fx`: an array of [`Effect`] entries.
    Effects,
    /// A string equal to one of the listed values.
    Enum(&'static [&'static str]),
}

/// A value shape: a body and whether `null` is also allowed ("not defined at this block", §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub body: Body,
    pub nullable: bool,
}

impl Shape {
    /// Checks `value` against this shape. The error is a short reason.
    pub fn check(&self, value: &Value) -> Result<(), &'static str> {
        if value.is_null() {
            return if self.nullable {
                Ok(())
            } else {
                Err("null is not allowed here")
            };
        }
        match self.body {
            Body::Int => value.as_int().map(drop).ok_or("expected an integer"),
            Body::U32 => match value.as_int() {
                Some(n) if u32::try_from(n).is_ok() => Ok(()),
                _ => Err("expected an unsigned 32-bit integer"),
            },
            Body::Bool => value.as_bool().map(drop).ok_or("expected a boolean"),
            Body::Str => value.as_bytes().map(drop).ok_or("expected a string"),
            Body::Hash => {
                value.as_hash().map(drop).ok_or("expected a hash (64 lowercase hex digits)")
            }
            Body::Qc => check_qc(value),
            Body::Cp => match value.as_arr() {
                Some([a, b, c, d])
                    if a.as_int().is_some()
                        && b.as_int().is_some()
                        && c.as_int().is_some()
                        && d.as_hash().is_some() =>
                {
                    Ok(())
                }
                _ => Err("expected [int, int, int, hash]"),
            },
            Body::Ehf => check_ehf(value),
            Body::Dep => check_dep(value),
            Body::Ints(n) => match value.as_arr() {
                Some(items) if items.len() == n && items.iter().all(|v| v.as_int().is_some()) => {
                    Ok(())
                }
                _ => Err("expected an array of the right number of integers"),
            },
            Body::Effects => Effect::list_from_value(value).map(drop).map_err(|e| e.0),
            Body::Enum(names) => match value.as_str() {
                Some(s) if names.contains(&s) => Ok(()),
                _ => Err("not one of the allowed names"),
            },
        }
    }
}

fn check_qc(value: &Value) -> Result<(), &'static str> {
    let rows = value.as_arr().ok_or("expected an array")?;
    let mut prev: Option<(i64, Hash256)> = None;
    for row in rows {
        let (t, q) = match row.as_arr() {
            Some([t, q, c]) if c.as_hash().is_some() => (t.as_int(), q.as_hash()),
            _ => return Err("expected [type, quorumHash, commitmentHash] rows"),
        };
        let (Some(t), Some(q)) = (t, q) else {
            return Err("expected [type, quorumHash, commitmentHash] rows");
        };
        if prev.is_some_and(|p| p >= (t, q)) {
            return Err("rows must be strictly ascending by type, then quorum hash");
        }
        prev = Some((t, q));
    }
    Ok(())
}

fn check_ehf(value: &Value) -> Result<(), &'static str> {
    let rows = value.as_arr().ok_or("expected an array")?;
    let mut prev: Option<i64> = None;
    for row in rows {
        let bit = match row.as_arr() {
            Some([bit, height]) if height.as_int().is_some() => bit.as_int(),
            _ => return Err("expected [bit, height] rows"),
        };
        let Some(bit) = bit else {
            return Err("expected [bit, height] rows");
        };
        if prev.is_some_and(|p| p >= bit) {
            return Err("rows must be strictly ascending by bit");
        }
        prev = Some(bit);
    }
    Ok(())
}

fn check_dep(value: &Value) -> Result<(), &'static str> {
    let rows = value.as_arr().ok_or("expected an array")?;
    for row in rows {
        match row.as_arr() {
            Some([name, state, since])
                if name.as_bytes().is_some()
                    && since.as_int().is_some()
                    && state.as_str().is_some_and(|s| DEPLOYMENT_STATES.contains(&s)) => {}
            _ => return Err("expected [name, state, since] rows with a known state"),
        }
    }
    Ok(())
}

/// One entry of [`FIELDS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldSpec {
    /// The record kind whose sub-object holds the key.
    pub kind: Kind,
    /// The key.
    pub key: &'static str,
    /// The class, which is also the sub-object: `s`, `o` or `x`.
    pub class: Class,
    /// Who writes it.
    pub source: FieldSource,
    /// The value shape.
    pub shape: Shape,
}

const fn req(body: Body) -> Shape {
    Shape {
        body,
        nullable: false,
    }
}

const fn opt(body: Body) -> Shape {
    Shape {
        body,
        nullable: true,
    }
}

const fn spec(
    kind: Kind,
    class: Class,
    key: &'static str,
    source: FieldSource,
    shape: Shape,
) -> FieldSpec {
    FieldSpec {
        kind,
        key,
        class,
        source,
        shape,
    }
}

const C: Kind = Kind::Connect;
const R: Kind = Kind::Reject;
const D: Kind = Kind::Disconnect;
const A: Kind = Kind::Attempt;
const S: Class = Class::Semantic;
const O: Class = Class::Observation;
const X: Class = Class::Schedule;
const P: FieldSource = FieldSource::Patch;
const G: FieldSource = FieldSource::Golden;

/// Every key of the `s`, `o` and `x` sub-objects, in spec order: the order the keys appear in a
/// canonical line. Entries of one `(kind, class)` group are contiguous. The era rules of §5
/// (which blocks may carry `null`) stay prose; the shape's `nullable` says only where `null` can
/// be legal at all.
pub const FIELDS: &[FieldSpec] = &[
    // C.s: header fields (§5.1), derived state (§5.2), golden statistics (§5.3).
    spec(C, S, "prev", P, opt(Body::Hash)),
    spec(C, S, "ver", P, req(Body::Int)),
    spec(C, S, "mrkl", P, req(Body::Hash)),
    spec(C, S, "time", P, req(Body::Int)),
    spec(C, S, "bits", P, req(Body::U32)),
    spec(C, S, "nonce", P, req(Body::U32)),
    spec(C, S, "work", P, req(Body::Hash)),
    spec(C, S, "ntx", P, req(Body::Int)),
    spec(C, S, "undo", P, opt(Body::Hash)),
    spec(C, S, "mnl", P, opt(Body::Hash)),
    spec(C, S, "qc", P, opt(Body::Qc)),
    spec(C, S, "cp", P, opt(Body::Cp)),
    spec(C, S, "ehf", P, opt(Body::Ehf)),
    spec(C, S, "bls", P, req(Body::Bool)),
    spec(C, S, "bur", P, req(Body::Int)),
    spec(C, S, "dep", P, req(Body::Dep)),
    spec(C, S, "muhash", G, req(Body::Str)),
    spec(C, S, "txouts", G, req(Body::Int)),
    spec(C, S, "amount", G, req(Body::Int)),
    spec(C, S, "unspendable", G, req(Body::Int)),
    spec(C, S, "bi", G, req(Body::Ints(7))),
    spec(C, S, "stats", G, opt(Body::Ints(7))),
    // C.o, C.x
    spec(C, O, "fx", P, req(Body::Effects)),
    spec(C, X, "seq", P, req(Body::Int)),
    // R
    spec(R, S, "res", P, req(Body::Enum(REJECT_RESULTS))),
    spec(R, S, "cls", P, opt(Body::Enum(REJECT_CLASSES))),
    spec(R, S, "why", P, opt(Body::Str)),
    spec(R, O, "fx", P, req(Body::Effects)),
    spec(R, X, "seq", P, req(Body::Int)),
    spec(R, X, "dbg", P, opt(Body::Str)),
    // D
    spec(D, S, "res", P, req(Body::Enum(DISCONNECT_RESULTS))),
    spec(D, O, "fx", P, req(Body::Effects)),
    spec(D, X, "seq", P, req(Body::Int)),
    // A (the decisions record has only the schedule sub-object)
    spec(A, X, "seq", P, req(Body::Int)),
];

/// The contiguous run of [`FIELDS`] for one `(kind, class)`; empty if there is none.
pub fn group(kind: Kind, class: Class) -> &'static [FieldSpec] {
    let Some(start) = FIELDS.iter().position(|f| f.kind == kind && f.class == class) else {
        return &[];
    };
    let len = FIELDS[start..].iter().take_while(|f| f.kind == kind && f.class == class).count();
    &FIELDS[start..start + len]
}

/// The spec of `key` in the `(kind, class)` sub-object.
pub fn lookup(kind: Kind, class: Class, key: &str) -> Option<&'static FieldSpec> {
    group(kind, class).iter().find(|f| f.key == key)
}

/// A key could not be stored in a [`Fields`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldError {
    /// The key is not defined for this record kind and sub-object.
    UnknownKey,
    /// The value does not have the key's shape.
    Shape(&'static str),
}

impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldError::UnknownKey => f.write_str("key is not defined for this sub-object"),
            FieldError::Shape(why) => write!(f, "wrong value shape: {why}"),
        }
    }
}

impl core::error::Error for FieldError {}

/// The keys present in one sub-object (`s`, `o` or `x`) of one record kind, with their values.
///
/// Absent is not `null`: a key that was never set is absent, and absence round-trips. Entries
/// are held in [`FIELDS`] order regardless of the order of [`Fields::set`] calls, and every
/// stored value has passed its shape check, so encoding a `Fields` always gives canonical bytes.
/// A `Fields` is created empty by the record constructors, for the kind and class of its
/// sub-object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fields {
    group: &'static [FieldSpec],
    /// `(index into group, value)`, ascending by index.
    entries: Vec<(usize, Value)>,
}

impl Fields {
    pub(crate) fn new(kind: Kind, class: Class) -> Fields {
        Fields {
            group: group(kind, class),
            entries: Vec::new(),
        }
    }

    /// Sets `key`, replacing and returning an earlier value. Fails if the key is not defined for
    /// this sub-object or the value has the wrong shape.
    fn set(&mut self, key: &str, value: Value) -> Result<Option<Value>, FieldError> {
        let idx = self.group.iter().position(|f| f.key == key).ok_or(FieldError::UnknownKey)?;
        self.group[idx].shape.check(&value).map_err(FieldError::Shape)?;
        Ok(self.put(idx, value))
    }

    /// Stores an already shape-checked value (the parser's path).
    pub(crate) fn put(&mut self, idx: usize, value: Value) -> Option<Value> {
        match self.entries.binary_search_by_key(&idx, |(i, _)| *i) {
            Ok(at) => Some(core::mem::replace(&mut self.entries[at].1, value)),
            Err(at) => {
                self.entries.insert(at, (idx, value));
                None
            }
        }
    }

    /// The value of `key`, if present (`Some(Value::Null)` for an explicit `null`).
    pub fn get(&self, key: &str) -> Option<&Value> {
        let idx = self.group.iter().position(|f| f.key == key)?;
        let at = self.entries.binary_search_by_key(&idx, |(i, _)| *i).ok()?;
        Some(&self.entries[at].1)
    }

    /// Removes `key`, making it absent.
    fn remove(&mut self, key: &str) -> Option<Value> {
        let idx = self.group.iter().position(|f| f.key == key)?;
        let at = self.entries.binary_search_by_key(&idx, |(i, _)| *i).ok()?;
        Some(self.entries.remove(at).1)
    }

    /// The present keys with their specs, in spec order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static FieldSpec, &Value)> + '_ {
        self.entries.iter().map(|(i, v)| (&self.group[*i], v))
    }

    /// Every key defined for this sub-object, present or not, in spec order.
    pub fn specs(&self) -> &'static [FieldSpec] {
        self.group
    }

    /// The number of present keys.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no key is present.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Appends the canonical encoding: `{"key":value,…}` with the present keys in spec order.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(b'{');
        for (n, (spec, value)) in self.iter().enumerate() {
            if n > 0 {
                out.push(b',');
            }
            out.push(b'"');
            out.extend_from_slice(spec.key.as_bytes());
            out.extend_from_slice(b"\":");
            value.encode(out);
        }
        out.push(b'}');
    }
}

/// Write access to one sub-object of a record, as handed out by `Outcome::s_mut` and its
/// siblings. It can set and remove keys, each checked against [`FIELDS`], but it cannot replace
/// the whole [`Fields`]: a sub-object never holds another sub-object's key set, so every record
/// built through this crate encodes to a line [`crate::parse_line`] accepts back.
#[derive(Debug)]
pub struct FieldsMut<'a>(&'a mut Fields);

impl<'a> FieldsMut<'a> {
    pub(crate) fn new(fields: &'a mut Fields) -> Self {
        FieldsMut(fields)
    }

    /// Sets `key`, replacing and returning an earlier value. Fails if the key is not defined for
    /// this sub-object or the value has the wrong shape.
    pub fn set(&mut self, key: &str, value: Value) -> Result<Option<Value>, FieldError> {
        self.0.set(key, value)
    }

    /// Removes `key`, making it absent.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.0.remove(key)
    }

    /// Sets `fx` from typed effects (a convenience over [`FieldsMut::set`]).
    pub fn set_effects(&mut self, effects: &[Effect]) -> Result<(), FieldError> {
        self.set("fx", Effect::list_to_value(effects)).map(drop)
    }

    /// Read access to the same sub-object.
    pub fn fields(&self) -> &Fields {
        self.0
    }
}

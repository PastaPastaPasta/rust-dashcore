//! The record model (spec §4) and its canonical writer.
//!
//! Spec §12 sketches `Record { kind, h, b, org, s, o, x, … }` with typed views. Here `Record` is
//! an enum over four shapes: [`Header`] (`H`), [`Outcome`] (`C`, `R` and `D`, which share one
//! body: identity plus the `s`, `o` and `x` sub-objects), [`Attempt`] (`A`) and [`Sparse`] (`S`).
//! [`Outcome::kind`] says which of the three it is. The `s`, `o` and `x` sub-objects are
//! [`Fields`], so a comparator reads every key and class generically.

use crate::fields::{Class, Fields, FieldsMut, Kind};
use crate::hex::Hash256;
use crate::typed::Query;
use crate::value::{Value, write_int, write_string};

/// The spec version this crate reads and writes (`H.v`).
pub const SPEC_VERSION: i64 = 1;

macro_rules! name_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            /// Every value, in spec order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The text the grammar uses.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            /// The value for its text.
            pub fn from_bytes(text: &[u8]) -> Option<$name> {
                match text {
                    $(t if t == $text.as_bytes() => Some($name::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

name_enum! {
    /// Where an attempt came from (spec §8.2).
    Origin {
        /// `ConnectTip` and `DisconnectTip`.
        Tip = "tip",
        /// `TestBlockValidity`.
        Proposal = "proposal",
        /// `VerifyDB`'s level-3 disconnects and level-4 reconnects.
        Verifydb = "verifydb",
        /// `ReplayBlocks`'s rollback disconnects.
        Replay = "replay",
    }
}

name_enum! {
    /// Which file an `H` record opens (spec §4.1).
    FileKind {
        Digest = "digest",
        Decisions = "decisions",
        Golden = "golden",
        /// A chain session's records (the oracle shim's in-memory sink, node-program O2b): the
        /// digest's and the decisions' records in one stream.
        Memory = "memory",
    }
}

name_enum! {
    /// Who wrote the file (`H.src`).
    Writer {
        Dashd = "dashd",
        Rdashd = "rdashd",
        Extract = "extract",
    }
}

name_enum! {
    /// Core's chain name (`H.chain`).
    Chain {
        Main = "main",
        Test = "test",
        Regtest = "regtest",
        Devnet = "devnet",
    }
}

/// `H`: the file header (spec §4.1). `v` is always [`SPEC_VERSION`]; the parser rejects any other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub file: FileKind,
    pub src: Writer,
    /// The writer's version string.
    pub ver: Vec<u8>,
    pub chain: Chain,
    /// `"<pid>-<unix microseconds at open>"` (schedule class); free text to the grammar.
    pub run: Vec<u8>,
}

/// The identity of an attempt's record: height, block hash and origin. Compared in every mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    pub h: i64,
    /// The block hash, display order.
    pub b: Hash256,
    pub org: Origin,
}

/// `C`, `R` or `D`: the outcome of one connect or disconnect attempt (spec §4.2-§4.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    kind: Kind,
    pub id: Ident,
    s: Fields,
    o: Fields,
    x: Fields,
}

impl Outcome {
    fn with_kind(kind: Kind, id: Ident) -> Outcome {
        Outcome {
            kind,
            id,
            s: Fields::new(kind, Class::Semantic),
            o: Fields::new(kind, Class::Observation),
            x: Fields::new(kind, Class::Schedule),
        }
    }

    /// A `C` record with no keys set yet.
    pub fn connect(id: Ident) -> Outcome {
        Outcome::with_kind(Kind::Connect, id)
    }

    /// An `R` record with no keys set yet.
    pub fn reject(id: Ident) -> Outcome {
        Outcome::with_kind(Kind::Reject, id)
    }

    /// A `D` record with no keys set yet.
    pub fn disconnect(id: Ident) -> Outcome {
        Outcome::with_kind(Kind::Disconnect, id)
    }

    /// `Kind::Connect`, `Kind::Reject` or `Kind::Disconnect`.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The semantic fields (`s`).
    pub fn s(&self) -> &Fields {
        &self.s
    }

    /// The observation fields (`o`).
    pub fn o(&self) -> &Fields {
        &self.o
    }

    /// The schedule fields (`x`).
    pub fn x(&self) -> &Fields {
        &self.x
    }

    /// The semantic fields, mutable.
    pub fn s_mut(&mut self) -> FieldsMut<'_> {
        FieldsMut::new(&mut self.s)
    }

    /// The observation fields, mutable.
    pub fn o_mut(&mut self) -> FieldsMut<'_> {
        FieldsMut::new(&mut self.o)
    }

    /// The schedule fields, mutable.
    pub fn x_mut(&mut self) -> FieldsMut<'_> {
        FieldsMut::new(&mut self.x)
    }

    /// The sub-object of `class`.
    pub fn fields(&self, class: Class) -> &Fields {
        match class {
            Class::Semantic => &self.s,
            Class::Observation => &self.o,
            Class::Schedule => &self.x,
        }
    }

    pub(crate) fn from_parts(kind: Kind, id: Ident, s: Fields, o: Fields, x: Fields) -> Outcome {
        Outcome {
            kind,
            id,
            s,
            o,
            x,
        }
    }
}

/// `A`: the decisions of one attempt (spec §4.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub id: Ident,
    /// `GetAdjustedTime()` in Unix seconds when the attempt opened.
    pub t: i64,
    /// The chain-helper queries in call order.
    pub q: Vec<Query>,
    x: Fields,
}

impl Attempt {
    /// An `A` record with no queries and no `x` keys set yet.
    pub fn new(id: Ident, t: i64) -> Attempt {
        Attempt {
            id,
            t,
            q: Vec::new(),
            x: Fields::new(Kind::Attempt, Class::Schedule),
        }
    }

    /// The schedule fields (`x`): `seq`.
    pub fn x(&self) -> &Fields {
        &self.x
    }

    /// The schedule fields, mutable.
    pub fn x_mut(&mut self) -> FieldsMut<'_> {
        FieldsMut::new(&mut self.x)
    }

    pub(crate) fn from_parts(id: Ident, t: i64, q: Vec<Query>, x: Fields) -> Attempt {
        Attempt {
            id,
            t,
            q,
            x,
        }
    }
}

/// Why a [`Sparse`] payload was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PayloadError(pub &'static str);

impl core::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

impl core::error::Error for PayloadError {}

/// `S`: one sparse golden RPC result (spec §4.6, §9.3).
///
/// `args` holds integers, strings, booleans, `null`, arrays and objects with sorted keys, but no
/// `Num`. `res` is a payload: every number is a `Num` (Core's text), objects have strictly
/// ascending byte-wise keys, and there is no `Int`. [`Sparse::new`] checks both, so every
/// `Sparse` encodes to a line the parser accepts back unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sparse {
    pub h: i64,
    /// The block hash, display order.
    pub b: Hash256,
    /// The RPC's name as the spec's §9.3 table spells it (free text to the grammar).
    pub rpc: Vec<u8>,
    args: Vec<Value>,
    res: Value,
}

impl Sparse {
    /// Builds a record, checking `args` and `res`.
    pub fn new(
        h: i64,
        b: Hash256,
        rpc: impl Into<Vec<u8>>,
        args: Vec<Value>,
        res: Value,
    ) -> Result<Sparse, PayloadError> {
        // The arguments sit one level down, inside the `args` array.
        for arg in &args {
            check_payload(arg, false, 1)?;
        }
        check_payload(&res, true, 0)?;
        Ok(Sparse {
            h,
            b,
            rpc: rpc.into(),
            args,
            res,
        })
    }

    pub(crate) fn from_parts(
        h: i64,
        b: Hash256,
        rpc: Vec<u8>,
        args: Vec<Value>,
        res: Value,
    ) -> Sparse {
        Sparse {
            h,
            b,
            rpc,
            args,
            res,
        }
    }

    /// The RPC arguments.
    pub fn args(&self) -> &[Value] {
        &self.args
    }

    /// The RPC result, re-encoded as a payload.
    pub fn res(&self) -> &Value {
        &self.res
    }
}

/// The deepest level a value may sit at inside a payload or an argument list, counting the
/// payload itself as level 0 (and `args` elements as level 1); the parser and `Sparse::new` apply
/// the same limit.
pub const MAX_DEPTH: usize = 128;

/// Whether `text` is in the number grammar `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`.
pub fn is_number_text(text: &str) -> bool {
    let b = text.as_bytes();
    let mut i = 0;
    if b.first() == Some(&b'-') {
        i += 1;
    }
    match b.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
        _ => return false,
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    i == b.len()
}

fn check_payload(value: &Value, payload: bool, depth: usize) -> Result<(), PayloadError> {
    if depth > MAX_DEPTH {
        return Err(PayloadError("nesting too deep"));
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Str(_) => Ok(()),
        Value::Int(_) if payload => Err(PayloadError("a payload holds numbers as Num, not Int")),
        Value::Int(_) => Ok(()),
        Value::Num(_) if !payload => Err(PayloadError("Num is legal only inside res")),
        Value::Num(text) if !is_number_text(text) => {
            Err(PayloadError("Num text is not in the number grammar"))
        }
        Value::Num(_) => Ok(()),
        Value::Arr(items) => items.iter().try_for_each(|v| check_payload(v, payload, depth + 1)),
        Value::Obj(members) => {
            if members.windows(2).any(|w| w[0].0 >= w[1].0) {
                return Err(PayloadError("object keys must be strictly ascending bytewise"));
            }
            members.iter().try_for_each(|(_, v)| check_payload(v, payload, depth + 1))
        }
    }
}

/// One record of any kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Header(Header),
    Outcome(Outcome),
    Attempt(Attempt),
    Sparse(Sparse),
}

impl Record {
    /// The record's kind.
    pub fn kind(&self) -> Kind {
        match self {
            Record::Header(_) => Kind::Header,
            Record::Outcome(o) => o.kind(),
            Record::Attempt(_) => Kind::Attempt,
            Record::Sparse(_) => Kind::Sparse,
        }
    }

    /// `(height, block hash)`; `None` for `H`.
    pub fn block(&self) -> Option<(i64, &Hash256)> {
        match self {
            Record::Header(_) => None,
            Record::Outcome(o) => Some((o.id.h, &o.id.b)),
            Record::Attempt(a) => Some((a.id.h, &a.id.b)),
            Record::Sparse(s) => Some((s.h, &s.b)),
        }
    }

    /// The origin; `None` for `H` and `S`.
    pub fn origin(&self) -> Option<Origin> {
        match self {
            Record::Outcome(o) => Some(o.id.org),
            Record::Attempt(a) => Some(a.id.org),
            Record::Header(_) | Record::Sparse(_) => None,
        }
    }

    /// The sub-object of `class`, for `C`, `R` and `D` (all three) and `A` (`x` only).
    pub fn fields(&self, class: Class) -> Option<&Fields> {
        match self {
            Record::Outcome(o) => Some(o.fields(class)),
            Record::Attempt(a) if class == Class::Schedule => Some(a.x()),
            _ => None,
        }
    }

    /// Appends the canonical line: the object and its `\n`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Record::Header(h) => {
                out.extend_from_slice(b"{\"k\":\"H\",\"v\":");
                write_int(out, SPEC_VERSION);
                out.extend_from_slice(b",\"file\":");
                write_string(out, h.file.as_str().as_bytes());
                out.extend_from_slice(b",\"src\":");
                write_string(out, h.src.as_str().as_bytes());
                out.extend_from_slice(b",\"ver\":");
                write_string(out, &h.ver);
                out.extend_from_slice(b",\"chain\":");
                write_string(out, h.chain.as_str().as_bytes());
                out.extend_from_slice(b",\"run\":");
                write_string(out, &h.run);
                out.push(b'}');
            }
            Record::Outcome(o) => {
                out.extend_from_slice(b"{\"k\":\"");
                out.push(o.kind.letter());
                out.push(b'"');
                write_ident(out, &o.id);
                out.extend_from_slice(b",\"s\":");
                o.s.encode(out);
                out.extend_from_slice(b",\"o\":");
                o.o.encode(out);
                out.extend_from_slice(b",\"x\":");
                o.x.encode(out);
                out.push(b'}');
            }
            Record::Attempt(a) => {
                out.extend_from_slice(b"{\"k\":\"A\"");
                write_ident(out, &a.id);
                out.extend_from_slice(b",\"t\":");
                write_int(out, a.t);
                out.extend_from_slice(b",\"q\":");
                Query::list_to_value(&a.q).encode(out);
                out.extend_from_slice(b",\"x\":");
                a.x.encode(out);
                out.push(b'}');
            }
            Record::Sparse(s) => {
                out.extend_from_slice(b"{\"k\":\"S\",\"h\":");
                write_int(out, s.h);
                out.extend_from_slice(b",\"b\":\"");
                s.b.write_hex(out);
                out.extend_from_slice(b"\",\"rpc\":");
                write_string(out, &s.rpc);
                out.extend_from_slice(b",\"args\":[");
                for (i, arg) in s.args.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    arg.encode(out);
                }
                out.extend_from_slice(b"],\"res\":");
                s.res.encode(out);
                out.push(b'}');
            }
        }
        out.push(b'\n');
    }

    /// The canonical line as a new buffer.
    pub fn to_line(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

fn write_ident(out: &mut Vec<u8>, id: &Ident) {
    out.extend_from_slice(b",\"h\":");
    write_int(out, id.h);
    out.extend_from_slice(b",\"b\":\"");
    id.b.write_hex(out);
    out.extend_from_slice(b"\",\"org\":\"");
    out.extend_from_slice(id.org.as_str().as_bytes());
    out.push(b'"');
}

impl From<Header> for Record {
    fn from(h: Header) -> Record {
        Record::Header(h)
    }
}

impl From<Outcome> for Record {
    fn from(o: Outcome) -> Record {
        Record::Outcome(o)
    }
}

impl From<Attempt> for Record {
    fn from(a: Attempt) -> Record {
        Record::Attempt(a)
    }
}

impl From<Sparse> for Record {
    fn from(s: Sparse) -> Record {
        Record::Sparse(s)
    }
}

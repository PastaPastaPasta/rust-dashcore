//! StateDigest v1 (node program task O8): the record model, the canonical JSON Lines grammar, the
//! writer and the strict parser. The contract is `docs/node-program/designs/O8-statedigest-v1.md`;
//! section numbers below (§) refer to it.
//!
//! The patched Dash Core writes these files, rdashd writes its own, and the conformance tools
//! read them, so all three share one grammar:
//!
//! * [`Value`]: the canonical JSON subset (§3), with byte-oriented strings;
//! * [`Record`] and its parts: `H`, `C`, `R`, `D`, `A` and `S` lines (§4), each field-for-field
//!   as the spec lists them; the `s`/`o`/`x` sub-objects are [`Fields`], validated against
//!   [`FIELDS`] (§5), the one table of keys, classes, sources and value shapes, in spec order;
//! * [`Effect`] and [`Query`]: the typed `fx` and `q` entries (§6, §7.1);
//! * [`Record::encode`]: the canonical line; [`parse_line`] and [`Lines`]: the strict inverse.
//!   A line is canonical iff re-encoding its parsed form gives the same bytes, and the parser
//!   enforces it (§3);
//! * [`recording::check_recording`]: the checks that bind one node's digest, decisions and golden
//!   files to each other (the `A` join, the tip-connect suffix);
//! * [`compare`]: the comparator of §10 (task O9a): the linear projection, the field-class rules,
//!   per-key coverage and the first mismatch;
//! * [`decisions`]: the recorded decisions as a replay source (§7.2, task O9a).
//! * [`emit`]: records as an emitter builds them from plain values (the header keys, the `R`
//!   and `D` results), for rdashd's tap (task O9a-2).
//!
//! Hashes are [`Hash256`]s in **display order**, the order of the printed hex (Core's
//! `uint256::GetHex()`), not the wire order.
//!
//! What the grammar checks is each key's own shape and place. Rules that relate fields (an `R`
//! with `res:"ok"` has `cls` and `why` null; `proposal` goes with a `fJustCheck` attempt) and the
//! era rules of §5 (which blocks may carry `null`) are the emitter's business, not the parser's.
//!
//! The crate is pure (node-pure group): no I/O, threads, clocks or async, and no dependencies.
//! The pure halves of O9a live here so that node crates can use them; the dump and the replay
//! driver, which read files and talk RPC, are in `dash-conformance`.

pub mod compare;
pub mod decisions;
pub mod emit;
mod fields;
mod hex;
mod parse;
mod record;
pub mod recording;
mod typed;
mod value;

pub use fields::{
    Body, Class, DEPLOYMENT_STATES, DISCONNECT_RESULTS, FIELDS, FieldError, FieldSource, FieldSpec,
    Fields, Kind, REJECT_CLASSES, REJECT_RESULTS, Shape, group, lookup,
};
pub use hex::{Hash256, decode_hex, write_hex};
pub use parse::{Lines, ParseError, ParseErrorKind, parse_line};
pub use record::{
    Attempt, Chain, FileKind, Header, Ident, MAX_DEPTH, Origin, Outcome, PayloadError, Record,
    SPEC_VERSION, Sparse, Writer, is_number_text,
};
pub use typed::{BestChainLock, Effect, IsConflictPair, Query, SbPayment, ShapeError};
pub use value::Value;

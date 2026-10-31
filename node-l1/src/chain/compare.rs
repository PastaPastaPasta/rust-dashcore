//! The planes (O2b §4): two systems' steps for one op, compared plane by plane. The reply as a
//! whole (class and canonical JSON); the C/R/D records in O9a's stream mode with `org`, less
//! the `H` records and less `org=verifydb` (counted, D17); the A records in strict sequence;
//! the events in order within the op; then the post-state field by field.
//!
//! O2b-3a runs Core alone, so this is exercised Core against Core (`l1_chain`'s replays) and by
//! injected differences; O2b-3b compares Core with `NodeSut` through it.
//!
//! Two choices 3b revisits with the node's side: the events plane is strict (the design counts
//! events only Core emits, which needs the node's event set first), and a records plane that
//! O9a finds incomplete (an op with no records on a side) passes, the other planes still
//! compared.

use std::convert::Infallible;

use dash_statedigest::compare::{
    FieldId, Mode, Options, Verdict, compare_attempts, compare_stream,
};
use dash_statedigest::{Attempt, Origin, Record, Sparse};

use super::step::Step;
use crate::Divergence;

/// What a comparison left out, for the census.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Skipped {
    /// `org=verifydb` outcome records, both sides (D17).
    pub verifydb: u64,
}

/// The records a side contributes, by plane.
struct Planes {
    /// Its outcome records (C/R/D) of compared origins, numbered in stream order.
    outcomes: Vec<(usize, Record)>,
    attempts: Vec<Attempt>,
    sparse: Vec<Sparse>,
}

fn split(records: &[Record], skipped: &mut Skipped) -> Planes {
    let mut planes = Planes {
        outcomes: Vec::new(),
        attempts: Vec::new(),
        sparse: Vec::new(),
    };
    for (at, r) in records.iter().enumerate() {
        match r {
            Record::Header(_) => {}
            Record::Outcome(o) if o.id.org == Origin::Verifydb => skipped.verifydb += 1,
            Record::Outcome(_) => planes.outcomes.push((at, r.clone())),
            Record::Attempt(a) => planes.attempts.push(a.clone()),
            Record::Sparse(s) => planes.sparse.push(s.clone()),
        }
    }
    planes
}

/// Compare one op's steps: the first plane of [`differences`], if any.
pub fn compare(left: &Step, right: &Step, exclude: &[FieldId]) -> Result<Skipped, Divergence> {
    let (skipped, differ) = differences(left, right, exclude);
    match differ.first() {
        None => Ok(skipped),
        Some(&field) => Err(Divergence {
            field,
        }),
    }
}

/// Every plane in which one op's steps differ, in the order they are read. `exclude`: the golden
/// keys (D16), left out of the record plane.
pub fn differences(left: &Step, right: &Step, exclude: &[FieldId]) -> (Skipped, Vec<&'static str>) {
    let mut skipped = Skipped::default();
    let mut differ = Vec::new();
    let mut plane = |equal: bool, field: &'static str| {
        if !equal {
            differ.push(field);
        }
    };
    plane(left.reply == right.reply, "reply");

    let lp = split(&left.records, &mut skipped);
    let rp = split(&right.records, &mut skipped);
    let mut opts = Options::new(Mode::Stream);
    opts.exclude = exclude.to_vec();
    let report = compare_stream::<Infallible>(
        lp.outcomes.into_iter().map(Ok),
        rp.outcomes.into_iter().map(Ok),
        opts,
    );
    plane(report.is_ok_and(|r| r.verdict != Verdict::Diverged), "records");
    plane(compare_attempts(&lp.attempts, &rp.attempts).is_none(), "attempts");
    plane(lp.sparse == rp.sparse, "sparse records");

    plane(left.events == right.events, "events");

    let (l, r) = (&left.post, &right.post);
    plane(l.down == r.down, "post.down");
    plane(l.flush_delta == r.flush_delta, "post.flush_delta");
    plane(l.persisted == r.persisted, "post.persisted");
    plane(l.tip == r.tip, "post.tip");
    plane(l.best_header == r.best_header, "post.best_header");
    plane(l.utxo == r.utxo, "post.utxo");
    plane(l.index == r.index, "post.index");
    plane(l.flushes == r.flushes, "post.flushes");
    plane(l.ibd == r.ibd, "post.ibd");
    plane(l.best_cl == r.best_cl, "post.best_cl");
    plane(l.sporks == r.sporks, "post.sporks");
    plane(l.tips == r.tips, "post.tips");
    plane(l.deployments == r.deployments, "post.deployments");
    (skipped, differ)
}

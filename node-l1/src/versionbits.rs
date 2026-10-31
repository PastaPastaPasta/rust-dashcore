//! CS4's `diff_versionbits`: Dash's versionbits state machine (`VersionBitsCache` with the
//! dynamic threshold and the EHF signal height) and `CMNHFManager::GetSignalsStage`, through
//! `oracle_versionbits_state` (ABI 12).
//!
//! The input is a scenario (`dash-consensus/tests/support/versionbits.rs`, shared with the
//! crate's replay of the recorded corpus): a chain selection, header runs, EHF signal changes and
//! queries run in order against one cache on each side. Compared: every answer, in order.

use dash_consensus::decode_header;
use dash_oracle_sys::{
    Checkpoints, CoinsView, Context, ErrorKind, MnList, Oracle, Pin, SignalChange, VbAnswer,
    VbQuery,
};

use crate::{Compared, Outcome, check, divergence};

// The scenario format and the node's side of its queries, shared with dash-consensus's tests.
#[allow(clippy::expect_used)]
#[path = "../../dash-consensus/tests/support/versionbits.rs"]
pub mod scenario;

use scenario::{Answer, Query, Refused, Scenario};

fn query(q: Query) -> VbQuery {
    let height = |h: Option<u32>| h.map(|h| h as i32);
    let pos = |p: dash_params::DeploymentPos| p.index() as u8;
    match q {
        Query::State(p, h) => VbQuery::State {
            pos: pos(p),
            height: height(h),
        },
        Query::StateSinceHeight(p, h) => VbQuery::StateSinceHeight {
            pos: pos(p),
            height: height(h),
        },
        Query::Statistics(p, h) => VbQuery::Statistics {
            pos: pos(p),
            height: height(h),
        },
        Query::ComputeBlockVersion(h) => VbQuery::ComputeBlockVersion {
            height: height(h),
        },
        Query::SignalsStage(h) => VbQuery::SignalsStage {
            height: height(h),
        },
    }
}

fn answer(a: VbAnswer) -> Answer {
    match a {
        VbAnswer::State(s) => Answer::State(s as u8),
        VbAnswer::StateSinceHeight(h) => Answer::StateSinceHeight(h),
        VbAnswer::Statistics(s) => Answer::Statistics {
            period: s.period,
            threshold: s.threshold,
            elapsed: s.elapsed,
            count: s.count,
            possible: s.possible,
            blocks: s.blocks,
        },
        VbAnswer::ComputeBlockVersion(v) => Answer::ComputeBlockVersion(v),
        VbAnswer::SignalsStage(s) => Answer::SignalsStage(s),
    }
}

/// The scenario's headers, linked by their X11 hashes.
pub fn headers(s: &Scenario) -> Vec<[u8; 80]> {
    scenario::headers_80(s, |h| match decode_header(&mut &h[..]) {
        Ok(header) => *header.value().hash().as_bytes(),
        Err(_) => unreachable!("80 bytes decode as a header"),
    })
}

/// Core's answers to the scenario's queries.
pub fn core_answers(s: &Scenario) -> dash_oracle_sys::Result<Vec<Answer>> {
    core_answers_for(s, headers(s))
}

/// Core's answers to the scenario's queries on `headers`, the 80-byte headers at
/// `s.first_height..` (a real chain's, whose fields are the scenario's runs).
pub fn core_answers_for(
    s: &Scenario,
    headers: Vec<[u8; 80]>,
) -> dash_oracle_sys::Result<Vec<Answer>> {
    let args = s.core_args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let chain = dash_oracle_sys::Chain::new(s.network.core_name(), &args);
    let ctx = Context::new(
        s.first_height as i32,
        headers,
        false,
        vec![],
        0,
        Checkpoints::Disabled,
        CoinsView::Unchecked(vec![]),
        MnList::Absent,
        vec![],
    );
    let signals: Vec<SignalChange> = s
        .signals
        .iter()
        .map(|(height, signals)| SignalChange {
            height: *height as i32,
            signals: signals.clone(),
        })
        .collect();
    let queries: Vec<VbQuery> = s.queries.iter().map(|&q| query(q)).collect();
    let answers = Oracle::new(Pin::V24_0).versionbits_state(&chain, &ctx, &signals, &queries)?;
    Ok(answers.into_iter().map(answer).collect())
}

/// The field an answer to `q` is compared under.
fn field(q: Query) -> &'static str {
    match q {
        Query::State(..) => "state",
        Query::StateSinceHeight(..) => "state since height",
        Query::Statistics(..) => "statistics",
        Query::ComputeBlockVersion(..) => "block version",
        Query::SignalsStage(..) => "signals stage",
    }
}

/// One scenario on both sides.
pub fn compare_scenario(s: &Scenario) -> Compared {
    let ours = s.ours();
    if ours == Err(Refused::Malformed) {
        return Ok(Outcome::OutOfDomain);
    }
    match (core_answers(s), ours) {
        (Ok(core), Ok(ours)) => {
            check(core.len() == ours.len(), "answer count")?;
            for ((q, core), ours) in s.queries.iter().zip(&core).zip(&ours) {
                check(core == ours, field(*q))?;
            }
            Ok(Outcome::BothAccept)
        }
        (Err(e), Err(Refused::Params)) if e.kind == ErrorKind::Network => Ok(Outcome::BothReject),
        (Err(e), Err(Refused::Period)) if e.kind == ErrorKind::Argument => Ok(Outcome::OutOfDomain),
        (Err(e), Err(Refused::Context)) if e.kind == ErrorKind::Context => Ok(Outcome::OutOfDomain),
        (Err(e), _) if e.kind == ErrorKind::Network => Err(divergence("chain parameters")),
        (Ok(_), Err(Refused::Params)) => Err(divergence("chain parameters")),
        (Err(e), _) if e.kind == ErrorKind::Context => Err(divergence("ehf context")),
        (Ok(_), Err(Refused::Context)) => Err(divergence("ehf context")),
        (Err(e), _) if e.kind == ErrorKind::Argument => Err(divergence("period")),
        (Ok(_), Err(Refused::Period)) => Err(divergence("period")),
        _ => Err(divergence("oracle error")),
    }
}

/// `diff_versionbits`: a scenario in the fuzz format (`Scenario::from_bytes`).
pub fn compare_versionbits(bytes: &[u8], pin: Pin) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    match Scenario::from_bytes(bytes) {
        Some(s) => compare_scenario(&s),
        None => Ok(Outcome::OutOfDomain),
    }
}

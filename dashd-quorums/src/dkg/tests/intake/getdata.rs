//! `NetDKG::ProcessGetData` (`l/net_dkg.cpp:706-748`, v24.0.0): each kind served from the
//! sessions' lookups, serialized under the scheme at the call.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use dash_llmq::dkg::bls::BlsSchemeKind;
use dash_llmq::dkg::framing::{check_wire_structure, max_dkg_message_size};
use dash_llmq::dkg::session::{DkgInv, DkgMsgKind, ServedSink};

use super::super::fakes::{params, tip};
use super::fixture::{Rig, TEST, round};
use crate::dkg::Served;
use crate::dkg::intake::message::payload_hash;

/// A kind, a recorded hash, a tip in a phase that serves it, and its payload under a scheme.
type Case = (DkgMsgKind, &'static [u8; 32], i32, Box<dyn Fn(BlsSchemeKind) -> Vec<u8>>);

const SCHEMES: [BlsSchemeKind; 2] = [BlsSchemeKind::Basic, BlsSchemeKind::Legacy];

fn inv(kind: DkgMsgKind, hash: &[u8; 32]) -> DkgInv {
    DkgInv {
        kind,
        hash: *hash,
    }
}

/// An active `llmq_test` intake whose handler holds a session that recorded the round's
/// messages, its premature commitment valid.
fn rig() -> Rig {
    let r = round();
    let rig = Rig::new(&[TEST]);
    let served = Arc::new(Served::new(Some(0)));
    let s = served.as_ref();
    ServedSink::contribution(s, r.contribution.session_hash(), &Arc::new(r.contribution.clone()));
    ServedSink::complaint(s, r.complaint.session_hash(), &Arc::new(r.complaint.clone()));
    ServedSink::justification(
        s,
        r.justification.session_hash(),
        &Arc::new(r.justification.clone()),
    );
    let hash = r.pcommit.session_hash();
    ServedSink::premature_commitment(s, hash, &Arc::new(r.pcommit.clone()));
    ServedSink::valid_commitment(s, hash);
    rig.sessions.handlers()[0].set_session(served);
    rig
}

#[test]
fn each_kind_is_served_in_its_phase_under_the_calls_scheme() {
    let r = round();
    let rig = rig();
    let cases: [Case; 4] = [
        (
            DkgMsgKind::Contribution,
            r.contribution.session_hash(),
            48,
            Box::new(|f| r.contribution.payload(f)),
        ),
        (
            DkgMsgKind::Complaint,
            r.complaint.session_hash(),
            52,
            Box::new(|f| r.complaint.payload(f)),
        ),
        (
            DkgMsgKind::Justification,
            r.justification.session_hash(),
            54,
            Box::new(|f| r.justification.payload(f)),
        ),
        (
            DkgMsgKind::PrematureCommitment,
            r.pcommit.session_hash(),
            56,
            Box::new(|f| r.pcommit.payload(f)),
        ),
    ];
    for (kind, hash, height, payload) in cases {
        rig.sessions.updated_block_tip(&tip(height), false);
        for flag in SCHEMES {
            assert_eq!(
                rig.intake.process_getdata(inv(kind, hash), flag == BlsSchemeKind::Legacy),
                Some((kind, payload(flag)))
            );
        }
        assert_eq!(rig.intake.process_getdata(inv(kind, &[0; 32]), false), None, "a miss");
        let other = if kind == DkgMsgKind::Contribution {
            DkgMsgKind::PrematureCommitment
        } else {
            DkgMsgKind::Contribution
        };
        assert_eq!(
            rig.intake.process_getdata(inv(other, hash), false),
            None,
            "{kind:?} as {other:?}"
        );
    }
}

#[test]
fn nothing_is_served_with_spork_17_off_or_to_an_observer() {
    let r = round();
    let rig = rig();
    rig.sessions.updated_block_tip(&tip(48), false);
    let contribution = inv(DkgMsgKind::Contribution, r.contribution.session_hash());
    assert!(rig.intake.process_getdata(contribution, false).is_some());
    rig.sporks.spork17.store(false, Ordering::SeqCst);
    assert_eq!(rig.intake.process_getdata(contribution, false), None);
    let observer = Rig::observer();
    assert_eq!(observer.intake.process_getdata(contribution, false), None);
}

/// Intake r1 m2: the round's session-serialized messages, under both schemes, pass
/// `ProcessMessage`'s size cap and framing walk, and under the session's scheme each payload's
/// inv hash is the session's. If the walk drifted from the serializer, every honest message
/// would score 100.
// Core: v24.0.0 src/llmq/net_dkg.cpp:44-76 MaxDKGMessageSize, :164-185
// CheckDKGMessageWireStructure, :629-632 NetDKG::ProcessMessage
#[test]
fn real_payloads_pass_the_cap_and_framing() {
    let r = round();
    let p = params(TEST);
    let mut matched = [false; 4];
    for flag in [BlsSchemeKind::Basic, BlsSchemeKind::Legacy] {
        let cases = [
            (
                DkgMsgKind::Contribution,
                r.contribution.payload(flag),
                *r.contribution.session_hash(),
            ),
            (DkgMsgKind::Complaint, r.complaint.payload(flag), *r.complaint.session_hash()),
            (
                DkgMsgKind::Justification,
                r.justification.payload(flag),
                *r.justification.session_hash(),
            ),
            (DkgMsgKind::PrematureCommitment, r.pcommit.payload(flag), *r.pcommit.session_hash()),
        ];
        for (i, (k, pl, h)) in cases.into_iter().enumerate() {
            assert!(pl.len() <= max_dkg_message_size(k, &p), "{k:?} {flag:?} size");
            assert!(check_wire_structure(k, &pl, &p), "{k:?} {flag:?} frame");
            matched[i] |= payload_hash(&pl) == h;
        }
    }
    assert_eq!(matched, [true; 4], "inv hash == payload hash under the session's scheme");
}

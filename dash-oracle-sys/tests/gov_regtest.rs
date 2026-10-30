//! The governance entries (ABI 26, O2a-GOV) on a Core-produced regtest network: GV1a's L1
//! evidence (`diff_gov`, `diff_gov_vote`). `scripts/gov_capture.py` ran Dash Core v24.0.0-rc.3's
//! own functional-test framework: four masternodes and two evonodes, four proposals (one left
//! unvoted), the masternodes' votes, a superblock trigger they created and voted on, then twelve
//! votes delivered over P2P to node 0 from fresh peers, each with the verdict node 0 logged. The
//! objects and votes replayed here are node 0's own, read from its `governance.dat`
//! (`dash-coreimport`); the fixture's provenance is in `tests/data/PROVENANCE.md`.
//!
//! What is compared with Core:
//! - every P2P probe's verdict (`MNGOVERNANCEOBJECTVOTE -- ... new` or `Rejected vote, error =
//!   ...`), the misbehaviour it scored and a malformed vote's decode failure, with the op script
//!   at the time node 0 logged;
//! - every object's weighted tallies and current votes (`gobject get`, `gobject
//!   getcurrentvotes`) before and after the probes;
//! - every object's hash, type, data and local validity (`fLocalValidity`, `IsValidReason`), and
//!   the trigger's payments (`getblocktemplate`);
//! - the superblock trigger at the next height and its payments (`getblocktemplate`), and the
//!   mined superblock's payments verdict under a scripted (format 2) and a format-1 context.

mod common;

use std::io::Read;
use std::sync::OnceLock;

use dash_coreimport::netstate::governance::{self, GovernanceObject, GovernanceVote};
use dash_network::Network;
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Collateral, Context, DEPLOYMENT_V24, ErrorKind, FlatFile,
    FulfilledOp, GovContext, GovExceptionType, GovOp, GovOutput, GovVote, MetaOp, MnList, NetAddr,
    ObjectState, Oracle, PaymentsContext, PaymentsScript, Service, SuperblockCheck, SyncOp,
    ThresholdState, Trigger, TxOut, VoteOutcome,
};
use serde_json::Value;

/// `getblocktemplate`'s and `gobject get`'s vote signals, in `vote_signal_enum_t` order.
const SIGNALS: [&str; 4] = ["Funding", "Valid", "Delete", "Endorsed"];
const OUTCOMES: [&str; 4] = ["none", "yes", "no", "abstain"];
const SIGNAL_NAMES: [&str; 5] = ["none", "funding", "valid", "delete", "endorsed"];

struct Fixture {
    json: Value,
    /// node 0's governance objects, keyed by hash (internal order).
    objects: Vec<([u8; 32], GovernanceObject)>,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
        let gz = std::fs::read(format!("{dir}/gov-regtest-v24.0.json.gz")).unwrap();
        let mut text = String::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_string(&mut text).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        let dat = std::fs::read(format!("{dir}/gov-regtest-v24.0.governance.dat")).unwrap();
        let loaded = governance::parse(&dat, Network::Regtest).unwrap();
        assert!(loaded.discarded.is_none() && loaded.value.version_matched);
        let objects = loaded.value.objects.into_iter().collect();
        Fixture {
            json,
            objects,
        }
    })
}

/// The node's chain parameters (the framework's dash.conf and gov_capture.py's arguments).
fn chain() -> Chain {
    Chain::new(
        "regtest",
        &[
            "-dip3params=2:2",
            "-testactivationheight=v20@100",
            "-testactivationheight=mn_rr@100",
            "-budgetparams=10:10:10",
        ],
    )
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

/// A CompactSize-prefixed byte string.
fn bytes(out: &mut Vec<u8>, b: &[u8]) {
    match b.len() {
        n @ 0..0xfd => out.push(n as u8),
        n @ 0xfd..=0xffff => {
            out.push(0xfd);
            out.extend((n as u16).to_le_bytes());
        }
        _ => unreachable!("no governance field is that long"),
    }
    out.extend(b);
}

/// `CGovernanceObject` in network serialization (governance/common.h:70-84).
fn object_bytes(o: &GovernanceObject) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(o.parent_hash);
    out.extend(o.revision.to_le_bytes());
    out.extend(o.time.to_le_bytes());
    out.extend(o.collateral_hash);
    bytes(&mut out, &o.data);
    out.extend(o.object_type.to_le_bytes());
    out.extend(o.masternode_outpoint.hash);
    out.extend(o.masternode_outpoint.n.to_le_bytes());
    bytes(&mut out, &o.signature);
    out
}

/// `CGovernanceVote` in network serialization (governance/vote.h:190-206).
fn vote_bytes(v: &GovernanceVote) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(v.masternode_outpoint.hash);
    out.extend(v.masternode_outpoint.n.to_le_bytes());
    out.extend(v.parent_hash);
    out.extend(v.outcome.to_le_bytes());
    out.extend(v.signal.to_le_bytes());
    out.extend(v.time.to_le_bytes());
    bytes(&mut out, &v.signature);
    out
}

fn tip() -> i32 {
    fixture().json["tip"].as_i64().unwrap() as i32
}

/// node 0's mock time at the tip.
fn mocktime_at_tip() -> i64 {
    fixture().json["mocktime_at_tip"].as_i64().unwrap()
}

/// The chain context at the block before the superblock, with node 0's masternode list there.
fn context() -> Context {
    context_at(mocktime_at_tip())
}

/// [`context`] with adjusted time `t`.
fn context_at(t: i64) -> Context {
    let j = &fixture().json;
    let headers = j["headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| common::hex(s(h)).try_into().unwrap())
        .collect();
    Context::new(
        0,
        headers,
        false,
        vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
        t,
        Checkpoints::Disabled,
        CoinsView::Unchecked(vec![]),
        MnList::Supplied(common::hex(s(&j["tip_list"]))),
        vec![],
    )
}

fn collaterals() -> Vec<Collateral> {
    let mut out: Vec<Collateral> = fixture().json["collaterals"]
        .as_object()
        .unwrap()
        .values()
        .map(|c| Collateral {
            tx: common::hex(s(&c["hex"])),
            block_hash: common::rpc_hash(s(&c["blockhash"])),
            active: true,
        })
        .collect();
    out.sort_by(|a, b| a.tx.cmp(&b.tx));
    out
}

fn gov_context() -> GovContext {
    GovContext {
        synced: true,
        collaterals: collaterals(),
    }
}

/// A probe's vote, as delivered.
fn probe_vote(probe: &Value) -> Vec<u8> {
    common::hex(s(&probe["vote"]))
}

/// The probe votes, as delivered.
fn probe_votes() -> Vec<Vec<u8>> {
    fixture().json["probes"].as_array().unwrap().iter().map(probe_vote).collect()
}

/// node 0's objects, proposals before triggers, and the votes it held before the probes (each
/// object's vote file in the order node 0 accepted them, without the probes it accepted).
fn objects_and_votes() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let probes = probe_votes();
    let mut objects: Vec<&GovernanceObject> = fixture().objects.iter().map(|(_, o)| o).collect();
    objects.sort_by_key(|o| o.object_type);
    let votes = objects
        .iter()
        .flat_map(|o| o.vote_file.votes.iter().map(vote_bytes))
        .filter(|v| !probes.contains(v))
        .collect();
    (objects.into_iter().map(object_bytes).collect(), votes)
}

/// node 0's mock time when it processed a probe, from the line it logged on receipt.
fn probe_time(probe: &Value) -> i64 {
    let log = probe["log"].as_array().unwrap();
    let line = log
        .iter()
        .map(s)
        .find(|l| {
            l.contains("MNGOVERNANCEOBJECTVOTE -- Received vote")
                || l.contains("malformed governance vote")
        })
        .unwrap_or_else(|| panic!("{}: no receipt line", probe["name"]));
    let t = line.strip_prefix("(mocktime: ").unwrap().split(')').next().unwrap();
    t.parse().unwrap()
}

/// The misbehaviour score node 0 gave the probe's peer, from `Misbehaving: peer=N (a -> b)`.
fn probe_penalty(probe: &Value) -> i32 {
    probe["log"]
        .as_array()
        .unwrap()
        .iter()
        .map(s)
        .find_map(|l| {
            let scores =
                l.split("Misbehaving: peer=").nth(1)?.split('(').nth(1)?.split(')').next()?;
            let (from, to) = scores.split_once(" -> ")?;
            Some(to.parse::<i32>().unwrap() - from.parse::<i32>().unwrap())
        })
        .unwrap_or(0)
}

/// Compare an object's state with `gobject get` and `gobject getcurrentvotes`.
fn check_state(name: &str, state: &ObjectState, get: &Value, current: &Value) {
    assert_eq!(state.object_type as i64, get["ObjectType"].as_i64().unwrap(), "{name}");
    for (tally, signal) in state.tallies.iter().zip(SIGNALS) {
        let result = &get[format!("{signal}Result")];
        assert_eq!(tally.yes as i64, result["YesCount"].as_i64().unwrap(), "{name} {signal} yes");
        assert_eq!(tally.no as i64, result["NoCount"].as_i64().unwrap(), "{name} {signal} no");
        assert_eq!(
            tally.abstain as i64,
            result["AbstainCount"].as_i64().unwrap(),
            "{name} {signal} abstain"
        );
        assert_eq!(
            (tally.yes - tally.no) as i64,
            result["AbsoluteYesCount"].as_i64().unwrap(),
            "{name} {signal} absolute"
        );
    }
    // GetCurrentVotes() prints each instance as a vote at its creation time:
    // "<outpoint>:<time>:<outcome>:<signal>:<weight>"; compare without the weight.
    let mut ours: Vec<String> = state
        .current_votes
        .iter()
        .flat_map(|((txid, n), instances)| {
            instances.iter().map(move |i| {
                format!(
                    "{}-{n}:{}:{}:{}",
                    common::rpc_hex(txid),
                    i.creation_time,
                    OUTCOMES[i.outcome as usize],
                    SIGNAL_NAMES[i.signal as usize]
                )
            })
        })
        .collect();
    let mut core: Vec<String> = current
        .as_object()
        .unwrap()
        .values()
        .map(|v| s(v).rsplit_once(':').unwrap().0.to_string())
        .collect();
    ours.sort();
    core.sort();
    assert_eq!(ours, core, "{name}: current votes");
}

fn states(outputs: &[GovOutput]) -> Vec<&ObjectState> {
    outputs
        .iter()
        .map(|o| match o {
            GovOutput::State(Some(state)) => &**state,
            other => panic!("{other:?}"),
        })
        .collect()
}

/// The getblocktemplate superblock payments as `CTxOut`s.
fn template_payments() -> Vec<TxOut> {
    fixture().json["getblocktemplate"]["superblock"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| TxOut {
            value: p["amount"].as_i64().unwrap(),
            script_pubkey: common::hex(s(&p["script"])),
        })
        .collect()
}

/// Compare a P2P vote probe's outcome with the verdict, message and misbehaviour node 0 logged.
fn check_probe(probe: &Value, outcome: GovOutput) {
    let name = s(&probe["name"]);
    let verdict = &probe["verdict"];
    if verdict["malformed"].as_bool() == Some(true) {
        assert_eq!(outcome, GovOutput::Vote(None), "{name}");
        assert_eq!(probe_penalty(probe), 100, "{name}");
        return;
    }
    let GovOutput::Vote(Some(VoteOutcome {
        hash,
        ok,
        exception,
        message,
        penalty,
        to_request,
    })) = outcome
    else {
        panic!("{name}: {outcome:?}")
    };
    assert_eq!(common::rpc_hex(&hash), s(&probe["hash"]), "{name}");
    assert_eq!(ok, verdict["accepted"].as_bool().unwrap(), "{name}");
    if ok {
        assert_eq!(
            (exception, message.as_str(), penalty),
            // The default CGovernanceException: its type's prefix and an empty message.
            (GovExceptionType::None, "GOVERNANCE_EXCEPTION_NONE:", 0),
            "{name}"
        );
    } else {
        assert_eq!(message, s(&verdict["error"]), "{name}");
        let prefix = match exception {
            GovExceptionType::None => "GOVERNANCE_EXCEPTION_NONE:",
            GovExceptionType::Warning => "GOVERNANCE_EXCEPTION_WARNING:",
            GovExceptionType::PermanentError => "GOVERNANCE_EXCEPTION_PERMANENT_ERROR:",
            GovExceptionType::TemporaryError => "GOVERNANCE_EXCEPTION_TEMPORARY_ERROR:",
            GovExceptionType::InternalError => "GOVERNANCE_EXCEPTION_INTERNAL_ERROR:",
        };
        assert!(message.starts_with(prefix), "{name}: {exception:?} {message}");
        assert_eq!(penalty, probe_penalty(probe), "{name}");
    }
    // An unknown parent is asked for (net_governance.cpp:259-264) only after the warning.
    let unknown_parent = message.contains("Unknown parent object");
    let parent: [u8; 32] = probe_vote(probe)[36..68].try_into().unwrap();
    assert_eq!(to_request, unknown_parent.then_some(parent), "{name}");
}

#[test]
fn replay_matches_node0() {
    let f = fixture();
    let j = &f.json;
    let chain = chain();
    let ctx = context();
    let gov = gov_context();
    let (objects, votes) = objects_and_votes();
    let hashes: Vec<[u8; 32]> = {
        let mut objs: Vec<&([u8; 32], GovernanceObject)> = f.objects.iter().collect();
        objs.sort_by_key(|(_, o)| o.object_type);
        objs.into_iter().map(|(h, _)| *h).collect()
    };
    assert_eq!(hashes.len(), j["gobject_list_all"].as_object().unwrap().len());

    for oracle in Oracle::all() {
        let mut ops = vec![GovOp::Time(mocktime_at_tip())];
        ops.extend(objects.iter().cloned().map(GovOp::Object));
        ops.extend(votes.iter().cloned().map(GovOp::Vote));
        ops.extend(hashes.iter().map(|h| GovOp::State(*h)));
        let probes = j["probes"].as_array().unwrap();
        for probe in probes {
            ops.push(GovOp::Time(probe_time(probe)));
            ops.push(GovOp::Vote(probe_vote(probe)));
        }
        ops.extend(hashes.iter().map(|h| GovOp::State(*h)));
        ops.push(GovOp::Superblock(tip() + 1));
        let mut out = oracle.gov_ops(&chain, &ctx, &gov, &ops).unwrap().into_iter();

        // Every object and every vote node 0 held is accepted again.
        for (hash, o) in hashes.iter().zip(out.by_ref().take(objects.len())) {
            let GovOutput::Object(Some(o)) = o else {
                panic!("{o:?}")
            };
            assert!(o.ok && o.held && !o.postponed && o.hash == *hash, "{o:?}");
        }
        for v in out.by_ref().take(votes.len()) {
            let GovOutput::Vote(Some(v)) = v else {
                panic!("{v:?}")
            };
            assert!(v.ok && v.exception == GovExceptionType::None, "{v:?}");
        }
        let before: Vec<GovOutput> = out.by_ref().take(hashes.len()).collect();
        for (hash, state) in hashes.iter().zip(states(&before)) {
            let key = common::rpc_hex(hash);
            check_state(&key, state, &j["gobject_get"][&key], &j["gobject_getcurrentvotes"][&key]);
        }

        // The probes: Core's verdict, message and misbehaviour for each.
        for probe in probes {
            check_probe(probe, out.next().unwrap());
        }

        let after: Vec<GovOutput> = out.by_ref().take(hashes.len()).collect();
        for (hash, state) in hashes.iter().zip(states(&after)) {
            let key = common::rpc_hex(hash);
            check_state(
                &format!("{key} after the probes"),
                state,
                &j["gobject_get_after_probes"][&key],
                &j["gobject_getcurrentvotes_after_probes"][&key],
            );
        }

        // The superblock: the trigger getblocktemplate pays.
        let GovOutput::Superblock(sb) = out.next().unwrap() else {
            panic!()
        };
        assert!(sb.triggered, "{sb:?}");
        let best = sb.best.expect("a best trigger");
        assert_eq!(f.objects.iter().find(|(h, _)| *h == best).unwrap().1.object_type, 2);
        assert_eq!(sb.payments, template_payments());
        assert!(out.next().is_none());
    }
}

#[test]
fn objects_match_node0() {
    let f = fixture();
    let j = &f.json;
    let chain = chain();
    let ctx = context();
    let gov = gov_context();
    let proposal_fee = 100_000_000; // getgovernanceinfo proposalfee 1.00000000
    for oracle in Oracle::all() {
        for (hash, o) in &f.objects {
            let key = common::rpc_hex(hash);
            let get = &j["gobject_get"][&key];
            let check = oracle.gov_object(&chain, &object_bytes(o), &ctx, &gov).unwrap();
            assert_eq!(check.hash, *hash, "{key}");
            assert_eq!(check.object_type as i64, get["ObjectType"].as_i64().unwrap(), "{key}");
            assert_eq!(
                (check.local_valid, check.local_error.as_str()),
                (get["fLocalValidity"].as_bool().unwrap(), s(&get["IsValidReason"])),
                "{key}"
            );
            // node 0 held it: ProcessObject() accepted it with the collateral checked.
            assert!(check.valid && !check.missing_confirmations, "{key}: {}", check.error);
            match o.object_type {
                1 => {
                    assert_eq!(check.min_fee, proposal_fee, "{key}");
                    assert!(check.trigger.is_err(), "{key}");
                }
                _ => {
                    assert_eq!(check.min_fee, 0, "{key}");
                    let trigger = check.trigger.as_ref().unwrap();
                    assert_eq!(trigger.height, tip() + 1);
                    assert!(trigger.valid_height);
                    let payments: Vec<TxOut> = trigger
                        .payments
                        .iter()
                        .map(|p| TxOut {
                            value: p.amount,
                            script_pubkey: p.script.clone(),
                        })
                        .collect();
                    assert_eq!(payments, template_payments(), "{key}");
                }
            }
            assert_eq!(common::to_hex(&o.data), s(&get["DataHex"]).to_lowercase(), "{key}");
        }
    }
}

#[test]
fn votes_check_the_tip_list() {
    let f = fixture();
    let chain = chain();
    // node 0's clock at the last probe: the vote from the future is at most an hour ahead of it
    // (vote.cpp:213-217), as when node 0 accepted it.
    let last = f.json["probes"].as_array().unwrap().iter().map(probe_time).max().unwrap();
    let ctx = context_at(last);
    let masternodes = f.json["masternodes"].as_array().unwrap();
    for oracle in Oracle::all() {
        // Every vote node 0 holds is valid with the key its signal needs: a funding vote on a
        // proposal with the voting key, every other with the operator key (object.cpp:458-466).
        for (_, o) in &f.objects {
            for v in &o.vote_file.votes {
                let check = oracle.gov_vote(&chain, &vote_bytes(v), &ctx).unwrap();
                assert!(check.reads_list && check.mn_found, "{check:?}");
                let voting_key = o.object_type == 1 && v.signal == 1;
                assert!(
                    if voting_key {
                        check.valid_voting
                    } else {
                        check.valid_operator
                    },
                    "{check:?}"
                );
                assert!(check.valid_unknown_parent, "{check:?}");
            }
        }
        // The probes against the masternode list at the tip.
        for (probe, vote) in f.json["probes"].as_array().unwrap().iter().zip(probe_votes()) {
            let name = s(&probe["name"]);
            let check = match oracle.gov_vote(&chain, &vote, &ctx) {
                Ok(check) => check,
                Err(e) => {
                    // The 10-byte signature: the network refuses it while decoding.
                    assert_eq!(
                        probe["verdict"]["malformed"].as_bool(),
                        Some(true),
                        "{name}: {e:?}"
                    );
                    continue;
                }
            };
            let txid: [u8; 32] = vote[..32].try_into().unwrap();
            let n = u32::from_le_bytes(vote[32..36].try_into().unwrap());
            let known = masternodes.iter().any(|m| {
                s(&m["collateral"][0]) == common::rpc_hex(&txid)
                    && m["collateral"][1].as_u64() == Some(n.into())
            });
            let signal = i32::from_le_bytes(vote[72..76].try_into().unwrap());
            assert_eq!(check.reads_list, (1..=4).contains(&signal), "{name}");
            assert_eq!(check.mn_found, check.reads_list && known, "{name}");
            let error = probe["verdict"]["error"].as_str().unwrap_or("");
            if error.contains("not found") || error.contains("Invalid vote for unknown parent") {
                // MG-02: an outpoint the list does not hold is never valid, whatever the parent.
                assert!(!check.mn_found && !check.valid_voting && !check.valid_operator, "{name}");
                assert!(!check.valid_unknown_parent, "{name}");
            }
            if name.starts_with("operator-signed funding") {
                assert!(check.mn_found && !check.valid_voting && check.valid_operator, "{name}");
                // Unknown parent: a funding vote falls back to the operator key (vote.cpp:244-250).
                assert!(check.valid_unknown_parent, "{name}");
            }
        }
    }
}

#[test]
fn superblock_payments_under_a_script() {
    let f = fixture();
    let j = &f.json;
    let chain = chain();
    let ctx = context();
    let block = common::hex(s(&j["superblock"]["hex"]));
    let (objects, votes) = objects_and_votes();
    let pay = PaymentsContext {
        fee_reward: 0,
        synced: true,
        best_chainlock_height: None,
        governance_loaded: true,
        triggers: vec![],
    };
    let trigger = f.objects.iter().find(|(_, o)| o.object_type == 2).map(|(_, o)| o).unwrap();
    let trigger_votes: Vec<Vec<u8>> = trigger.vote_file.votes.iter().map(vote_bytes).collect();
    for oracle in Oracle::all() {
        let script = |votes: &[Vec<u8>]| PaymentsScript {
            collaterals: collaterals(),
            ops: std::iter::once(GovOp::Time(mocktime_at_tip()))
                .chain(objects.iter().cloned().map(GovOp::Object))
                .chain(votes.iter().cloned().map(GovOp::Vote))
                .collect(),
        };
        // node 0's objects and votes: the block it mined is valid.
        let held =
            oracle.block_value_payee_scripted(&chain, &block, &ctx, &pay, &script(&votes)).unwrap();
        assert!(held.verdict.is_accept() && held.value_valid, "{held:?}");
        assert_eq!(held.check, SuperblockCheck::AllowDuplicates);
        // The same trigger as a format-1 context gives the same answer.
        let format1 = PaymentsContext {
            triggers: vec![Trigger {
                object: object_bytes(trigger),
                votes: trigger
                    .vote_file
                    .votes
                    .iter()
                    .map(|v| GovVote {
                        collateral_txid: v.masternode_outpoint.hash,
                        collateral_vout: v.masternode_outpoint.n,
                        signal: v.signal as u8,
                        outcome: v.outcome as u8,
                    })
                    .collect(),
            }],
            ..pay.clone()
        };
        assert_eq!(oracle.block_value_payee(&chain, &block, &ctx, &format1).unwrap(), held);
        // Without the trigger's votes nothing is triggered and the superblock overpays (derived
        // from IsBlockValueValid(), not captured: node 0 never saw that state).
        let unvoted: Vec<Vec<u8>> =
            votes.iter().filter(|v| !trigger_votes.contains(v)).cloned().collect();
        let bare = oracle
            .block_value_payee_scripted(&chain, &block, &ctx, &pay, &script(&unvoted))
            .unwrap();
        assert!(!bare.value_valid && !bare.verdict.is_accept(), "{bare:?}");
        // A script's triggers replace format 1's.
        let err = oracle
            .block_value_payee_scripted(&chain, &block, &ctx, &format1, &script(&votes))
            .unwrap_err();
        assert_eq!(err.kind, dash_oracle_sys::ErrorKind::Argument);
    }
}

/// [`context`] cut at `height`, without a masternode list (a proposal's checks read none).
fn context_to(height: i32) -> Context {
    let headers = fixture().json["headers"].as_array().unwrap()[..=height as usize]
        .iter()
        .map(|h| common::hex(s(h)).try_into().unwrap())
        .collect();
    Context::new(
        0,
        headers,
        false,
        vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
        mocktime_at_tip(),
        Checkpoints::Disabled,
        CoinsView::Unchecked(vec![]),
        MnList::Absent,
        vec![],
    )
}

/// IsCollateralValid() counts confirmations on the context's headers (object.cpp:855-875):
/// six make a proposal valid, one to five postpone it (`missing_confirmations`, which
/// ProcessObject() turns into a postponed object), none reject it. Expectations derived from
/// those lines, every answer the oracle's; and the governance context's refusals.
#[test]
fn collateral_confirmations_and_refusals() {
    let f = fixture();
    let chain = chain();
    let (hash, proposal) = f.objects.iter().find(|(_, o)| o.object_type == 1).unwrap();
    let txid = common::rpc_hex(&proposal.collateral_hash);
    let mined = &f.json["collaterals"][&txid];
    let height = mined["height"].as_i64().unwrap() as i32;
    let collateral = Collateral {
        tx: common::hex(s(&mined["hex"])),
        block_hash: common::rpc_hash(s(&mined["blockhash"])),
        active: true,
    };
    let gov = |collaterals: Vec<Collateral>| GovContext {
        synced: true,
        collaterals,
    };
    let object = object_bytes(proposal);
    for oracle in Oracle::all() {
        let check = |tip: i32, c: Collateral| {
            oracle.gov_object(&chain, &object, &context_to(tip), &gov(vec![c]))
        };
        for (confirmations, valid, missing) in
            [(1, false, true), (5, false, true), (6, true, false)]
        {
            let o = check(height + confirmations - 1, collateral.clone()).unwrap();
            assert_eq!(o.hash, *hash);
            assert!(o.local_valid, "{confirmations}: {}", o.local_error);
            assert_eq!(
                (o.valid, o.missing_confirmations),
                (valid, missing),
                "{confirmations}: {}",
                o.error
            );
            if !valid {
                assert_eq!(o.error, "Invalid proposal collateral", "{confirmations}");
            }
        }
        // Off the active chain: no confirmations at all.
        let off_chain = Collateral {
            block_hash: [0x11; 32],
            active: false,
            ..collateral.clone()
        };
        let o = check(tip(), off_chain).unwrap();
        assert!(!o.valid && !o.missing_confirmations, "{o:?}");

        // Refusals: an active block the headers do not reach is missing context; a null block
        // hash, an inactive block that is a header and a repeated transaction are arguments.
        assert_eq!(check(height - 1, collateral.clone()).unwrap_err().kind, ErrorKind::Context);
        for (name, collaterals) in [
            (
                "null block",
                vec![Collateral {
                    block_hash: [0; 32],
                    ..collateral.clone()
                }],
            ),
            (
                "inactive header",
                vec![Collateral {
                    active: false,
                    ..collateral.clone()
                }],
            ),
            ("repeated", vec![collateral.clone(), collateral.clone()]),
        ] {
            let err =
                oracle.gov_object(&chain, &object, &context(), &gov(collaterals)).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Argument, "{name}: {err:?}");
        }
        // A clock of 0 is the wall clock (util/time.cpp:29-33): refused for every entry.
        let zero = context_at(0);
        assert_eq!(
            oracle.gov_object(&chain, &object, &zero, &gov_context()).unwrap_err().kind,
            ErrorKind::Argument
        );
        let vote = vote_bytes(&proposal.vote_file.votes[0]);
        assert_eq!(oracle.gov_vote(&chain, &vote, &zero).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(
            oracle.gov_ops(&chain, &zero, &gov_context(), &[]).unwrap_err().kind,
            ErrorKind::Argument
        );
    }
}

/// Set in the child process `oracle_writes_no_file_outside_its_temp_dir` runs.
const STRAY_CHILD: &str = "DASH_ORACLE_STRAY_CHILD";

/// Every file (not directory) under `dir`, relative to it.
fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                todo.push(path);
            } else {
                out.push(path.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    out
}

/// The networks the stray-file child runs BL-451's entries on, with each one's directory under
/// Core's default data directory (`BaseParams().DataDir()`; a devnet's is `devnet-<name>`).
fn stray_networks() -> [(Chain, &'static str); 4] {
    [
        (Chain::main(), ""),
        (Chain::test(), "testnet3"),
        (chain(), "regtest"),
        (Chain::devnet("bl451"), "devnet-bl451"),
    ]
}

/// BL-477: the governance node (a valid `CMasternodeMetaMan`, which `ProcessVote()` asserts),
/// under each of its callers (the object, vote and ops entries and the payments entry), and
/// BL-451's sync, fulfilled-request, meta-cache and flat-file entries on every network leave
/// nothing in Core's default data directory (`$HOME/.dashcore`), in `$TMPDIR` or in the working
/// directory (where a store lands when the data directory is empty) once they return. Core caches
/// the data directory's path on first use, so the calls run in a child process of this test
/// binary with all three pointing at directories of its own. `CFlatDB::Store()` opens its file
/// without creating a directory (flat-database.h), so the default data directory and the
/// directory of every network the child runs on exist, empty, as on a machine that has run
/// dashd: a missing one would let a store fail silently. The child writes `STRAY_CHILD`'s file
/// last, so a child that ran nothing fails the test.
#[test]
fn oracle_writes_no_file_outside_its_temp_dir() {
    if let Some(done) = std::env::var_os(STRAY_CHILD) {
        let f = fixture();
        let chain = chain();
        let (objects, votes) = objects_and_votes();
        let block = common::hex(s(&f.json["superblock"]["hex"]));
        let pay = PaymentsContext {
            fee_reward: 0,
            synced: true,
            best_chainlock_height: None,
            governance_loaded: true,
            triggers: vec![],
        };
        let mut ops = vec![GovOp::Time(mocktime_at_tip())];
        ops.extend(objects.iter().cloned().map(GovOp::Object));
        ops.extend(votes.iter().cloned().map(GovOp::Vote));
        let script = PaymentsScript {
            collaterals: collaterals(),
            ops: ops.clone(),
        };
        for oracle in Oracle::all() {
            let (_, o) = &f.objects[0];
            oracle.gov_object(&chain, &object_bytes(o), &context(), &gov_context()).unwrap();
            oracle.gov_vote(&chain, &votes[0], &context()).unwrap();
            oracle.gov_ops(&chain, &context(), &gov_context(), &ops).unwrap();
            oracle.block_value_payee_scripted(&chain, &block, &context(), &pay, &script).unwrap();
            for (net, _) in stray_networks() {
                let mn = [7; 32];
                let meta = [MetaOp::AllowMixing(mn), MetaOp::AddGovernanceVote(mn, [9; 32])];
                oracle.mncache_step(&net, &meta).unwrap();
                let sync = [
                    SyncOp::Time(mocktime_at_tip()),
                    SyncOp::BumpAttempt,
                    SyncOp::SwitchToNextAsset,
                ];
                oracle.mnsync_step(&net, mocktime_at_tip(), &sync).unwrap();
                let peer = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19899);
                let fulfilled = [
                    FulfilledOp::Add(peer, "full-sync".into()),
                    FulfilledOp::CheckAndRemove,
                    FulfilledOp::DoMaintenance,
                ];
                oracle.netfulfilled_step(&net, mocktime_at_tip(), &fulfilled).unwrap();
                for kind in [FlatFile::NetFulfilled, FlatFile::MnCache] {
                    oracle.flatdb_load(&net, kind, mocktime_at_tip(), None).unwrap();
                }
            }
        }
        std::fs::write(done, b"").unwrap();
        return;
    }
    let root = std::env::temp_dir().join(format!("oracle-stray-{}", std::process::id()));
    let (home, tmp, cwd) = (root.join("home"), root.join("tmp"), root.join("cwd"));
    for (_, dir) in stray_networks() {
        std::fs::create_dir_all(home.join(".dashcore").join(dir)).unwrap();
    }
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    let done = root.join("done");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "oracle_writes_no_file_outside_its_temp_dir", "--test-threads", "1"])
        .env(STRAY_CHILD, &done)
        .env("HOME", &home)
        .env("TMPDIR", &tmp)
        .current_dir(&cwd)
        .status()
        .unwrap();
    let left = (files_under(&home), files_under(&tmp), files_under(&cwd));
    let ran = done.exists();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(status.success(), "the child failed: {status}");
    assert!(ran, "the child ran no oracle call");
    assert_eq!(
        left,
        (vec![], vec![], vec![]),
        "files the oracle left in $HOME, $TMPDIR and the cwd"
    );
}

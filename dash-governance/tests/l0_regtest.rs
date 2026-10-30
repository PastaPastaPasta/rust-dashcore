//! L0: the Core-produced regtest network of O2a-GOV (`dash-oracle-sys/tests/data/
//! gov-regtest-v24.0.*`, provenance in that directory's `PROVENANCE.md`) replayed through the
//! crate with no oracle (notes D15). node 0's objects and votes are read from its
//! `governance.dat`; every answer compared is what node 0's RPC printed: `gobject get` (hash,
//! type, local validity and reason, the four signals' tallies, the cached flags), `gobject
//! getcurrentvotes`, and `getblocktemplate`'s superblock payments at the next superblock height.

use std::collections::BTreeMap;

use dash_consensus::Tx;
use dash_governance::GovParams;
use dash_governance::exception::ExceptionType;
use dash_governance::object::{self, Check, TRIGGER, is_valid_locally};
use dash_governance::strings::{hash_hex, hex_str};
use dash_governance::superblocks::SuperblockSet;
use dash_governance::tally::{ObjectState, UPDATE_MIN};
use dash_governance::trigger::Superblock;
use dash_governance::vote;
use dash_primitives::{GovObject, Transaction, TxType};
use serde_json::Value;

mod common;

use common::{fixture, params, unhex};

const SIGNALS: [(i32, &str); 4] = [
    (vote::SIGNAL_FUNDING, "Funding"),
    (vote::SIGNAL_VALID, "Valid"),
    (vote::SIGNAL_DELETE, "Delete"),
    (vote::SIGNAL_ENDORSED, "Endorsed"),
];

#[test]
fn node0_objects_votes_tallies_and_superblock() {
    let fx = fixture();
    let p = params();
    // node 0's clock after the last probe: it accepted "a vote from the future" at that time.
    let now = fx.json["probes"]
        .as_array()
        .expect("probes")
        .iter()
        .map(|p| p["time"].as_i64().expect("a probe's time"))
        .max()
        .expect("probes");
    let tip = fx.json["tip"].as_i64().expect("tip") as i32;
    let tip_time = fx.json["mocktime_at_tip"].as_i64().expect("mocktime");
    let probes: Vec<&str> = fx.json["probes"]
        .as_array()
        .expect("probes")
        .iter()
        .map(|p| p["hash"].as_str().expect("a probe's hash"))
        .collect();
    let get = &fx.json["gobject_get_after_probes"];
    let current = &fx.json["gobject_getcurrentvotes_after_probes"];
    assert_eq!(fx.objects.len(), get.as_object().expect("objects").len());
    let mut states = BTreeMap::new();
    let mut set = SuperblockSet::new();
    let mut votes_replayed = 0;
    for (obj, votes) in &fx.objects {
        let hash = object::hash(obj);
        let shown = hash_hex(&hash);
        let rpc = &get[&shown];
        assert!(rpc.is_object(), "{shown}: not in gobject get");
        let obj_type = object::object_type(obj);
        assert_eq!(i64::from(obj_type), rpc["ObjectType"].as_i64().expect("type"), "{shown}");
        assert_eq!(hex_str(&obj.data), rpc["DataHex"].as_str().expect("data"), "{shown}");
        let local = is_valid_locally(obj, false, Check::NoCollateral, &p);
        assert_eq!(local.valid, rpc["fLocalValidity"].as_bool().expect("validity"), "{shown}");
        assert_eq!(local.error, rpc["IsValidReason"].as_str().expect("reason"), "{shown}");
        // node 0 accepted each vote of the file in this order; replayed without the rate check
        // (as `CheckOrphanVotes` and a reload do), each is accepted again.
        let mut state = ObjectState::new(obj_type, hash);
        for v in votes {
            state.process_vote(v, &fx.list, false, now, &p).unwrap_or_else(|e| panic!("{e}"));
            votes_replayed += 1;
        }
        for (signal, name) in SIGNALS {
            let r = &rpc[format!("{name}Result")];
            let n = |k: &str| r[k].as_i64().expect("count") as i32;
            assert_eq!(state.yes(&fx.list, signal), n("YesCount"), "{shown} {name} yes");
            assert_eq!(state.no(&fx.list, signal), n("NoCount"), "{shown} {name} no");
            assert_eq!(state.abstain(&fx.list, signal), n("AbstainCount"), "{shown} {name}");
            assert_eq!(state.absolute_yes(&fx.list, signal), n("AbsoluteYesCount"), "{shown}");
        }
        // "<outpoint>:<time>:<outcome>:<signal>:<weight>", compared without the weight.
        let mut ours: Vec<String> = state
            .current
            .iter()
            .flat_map(|((txid, n), instances)| {
                instances.iter().map(move |(&signal, i)| {
                    let mut display = *txid;
                    display.reverse();
                    format!(
                        "{}-{n}:{}:{}:{}",
                        hex_str(&display),
                        i.creation_time,
                        vote::outcome_to_string(i.outcome),
                        vote::signal_to_string(signal)
                    )
                })
            })
            .collect();
        let mut core: Vec<String> = current[&shown]
            .as_object()
            .expect("current votes")
            .values()
            .map(|v| v.as_str().expect("a vote").rsplit_once(':').expect("weight").0.to_owned())
            .collect();
        ours.sort();
        core.sort();
        assert_eq!(ours, core, "{shown}: current votes");
        // The cached flags `gobject get` printed before the probes: node 0 updates them in its
        // periodic clean, which had not run since the probes (O2a-GOV D7). Without the probes'
        // votes, at the tip's time.
        let mut before = ObjectState::new(obj_type, hash);
        for v in votes.iter().filter(|v| !probes.contains(&hash_hex(&vote::hash(v)).as_str())) {
            before.process_vote(v, &fx.list, false, tip_time, &p).unwrap_or_else(|e| panic!("{e}"));
        }
        before.update_sentinel(&fx.list, tip_time, &p);
        let rpc_before = &fx.json["gobject_get"][&shown];
        let flag = |k: &str| rpc_before[k].as_bool().expect("flag");
        assert_eq!(
            (
                before.cached_funding,
                before.cached_valid,
                before.cached_delete,
                before.cached_endorsed
            ),
            (
                flag("fCachedFunding"),
                flag("fCachedValid"),
                flag("fCachedDelete"),
                flag("fCachedEndorsed")
            ),
            "{shown}: cached flags"
        );
        for (signal, name) in SIGNALS {
            let yes = rpc_before[format!("{name}Result")]["YesCount"].as_i64().expect("count");
            assert_eq!(i64::from(before.yes(&fx.list, signal)), yes, "{shown} {name} before");
        }
        state.update_sentinel(&fx.list, now, &p);
        if obj_type == TRIGGER {
            let sb = Superblock::parse(obj, &p).expect("node 0's trigger parses");
            assert_eq!(sb.height, 180);
            check_trigger(obj, &sb, &fx.json, &p);
            assert_eq!(set.add_trigger(obj, tip, &p), Ok(true), "a live trigger");
        }
        states.insert(hash, state);
    }
    assert!(votes_replayed > 20, "{votes_replayed} votes");
    // The next superblock: triggered, and the best trigger's payments are getblocktemplate's.
    assert!(set.is_triggered(&fx.list, 180, &mut states, now, &p));
    let payments = set.payments(&fx.list, 180, &states, &p).expect("a best trigger");
    let template: Vec<(i64, String)> = fx.json["getblocktemplate"]["superblock"]
        .as_array()
        .expect("superblock payments")
        .iter()
        .map(|t| {
            (
                t["amount"].as_i64().expect("amount"),
                t["script"].as_str().expect("script").to_owned(),
            )
        })
        .collect();
    let ours: Vec<(i64, String)> =
        payments.iter().map(|o| (o.value, hex_str(o.script_pubkey.as_bytes()))).collect();
    assert_eq!(ours, template);
    assert!(set.payments(&fx.list, 160, &states, &p).is_none());
    assert!(set.payments(&fx.list, 181, &states, &p).is_none());
}

/// node 0's vote files rebuilt, oldest vote first: the same votes in the same order (newest
/// first, `votedb.cpp:32-43`), each a known vote on a second try (`object.cpp:383-389`); and the
/// hourly limit (`object.cpp:446`) on a re-timed copy of a vote, which reaches the rate check
/// before its signature fails.
#[test]
fn node0_vote_files_and_the_hourly_limit() {
    let fx = fixture();
    let p = params();
    // `GOVERNANCE_UPDATE_MIN`, one hour (`object.h:91`), written out so a changed constant fails.
    const HOUR: i64 = 3600;
    assert_eq!(UPDATE_MIN, HOUR);
    let now = fx.json["mocktime_at_tip"].as_i64().expect("mocktime") + 2 * HOUR;
    let mut files = 0;
    for (obj, votes) in &fx.objects {
        let mut state = ObjectState::new(object::object_type(obj), object::hash(obj));
        assert!(state.file.is_empty());
        for v in votes.iter().rev() {
            state.process_vote(v, &fx.list, false, now, &p).unwrap_or_else(|e| panic!("{e}"));
        }
        assert_eq!(state.file.len(), votes.len());
        assert_eq!(state.file.is_empty(), votes.is_empty());
        let ours: Vec<[u8; 32]> = state.file.votes().map(vote::hash).collect();
        let core: Vec<[u8; 32]> = votes.iter().map(vote::hash).collect();
        assert_eq!(ours, core, "{}: vote file", hash_hex(&state.hash));
        for v in votes {
            assert!(state.file.has(&vote::hash(v)));
            let e = state.process_vote(v, &fx.list, false, now, &p).expect_err("a known vote");
            assert_eq!(e.kind(), ExceptionType::None);
            assert!(e.message().ends_with("Already known valid vote"), "{e}");
        }
        files += usize::from(!votes.is_empty());
    }
    assert!(files > 1);

    let (obj, votes) = fx.objects.iter().find(|(_, v)| !v.is_empty()).expect("a voted object");
    let v = &votes[0];
    let mut state = ObjectState::new(object::object_type(obj), object::hash(obj));
    state.process_vote(v, &fx.list, true, now, &p).unwrap_or_else(|e| panic!("{e}"));
    let mut later = v.clone();
    later.time += 1;
    let e = state.process_vote(&later, &fx.list, true, now + HOUR - 1, &p).expect_err("hourly");
    assert_eq!(e.kind(), ExceptionType::TemporaryError, "{e}");
    assert!(e.message().contains("Masternode voting too often"), "{e}");
    let e = state.process_vote(&later, &fx.list, true, now + HOUR, &p).expect_err("signature");
    assert_eq!(e.kind(), ExceptionType::PermanentError, "{e}");
    assert!(e.message().contains("Invalid vote, MN outpoint"), "{e}");
}

/// node 0's trigger: its total and data as `CSuperblock` writes them, and `IsValid` on a coinbase
/// paying getblocktemplate's superblock outputs (`superblock.cpp:237-240, 248-334, 389-413`).
fn check_trigger(obj: &GovObject, sb: &Superblock, json: &Value, p: &GovParams) {
    let template = json["getblocktemplate"]["superblock"].as_array().expect("payments");
    let total: i64 = template.iter().map(|t| t["amount"].as_i64().expect("amount")).sum();
    assert!(total > 0);
    assert_eq!(sb.total(), total);
    let data = unhex(&sb.hex_str_data(p));
    let rewritten = GovObject {
        data,
        ..obj.clone()
    };
    let reparsed = Superblock::parse(&rewritten, p).expect("GetHexStrData parses");
    assert_eq!(reparsed.height, sb.height);
    assert_eq!(reparsed.tx_outs(), sb.tx_outs());
    let reward = json["getblocktemplate"]["coinbasevalue"].as_i64().expect("coinbasevalue");
    let coinbase = |outputs| {
        Tx::new(Transaction {
            version: 3,
            tx_type: TxType::new(5),
            inputs: vec![],
            outputs,
            lock_time: 0,
            extra_payload: vec![],
        })
    };
    let height = sb.height as u32;
    assert_eq!(sb.is_valid(&coinbase(sb.tx_outs()), height, reward, true, p), Ok(true));
    let mut short = sb.tx_outs();
    short.pop();
    assert_eq!(sb.is_valid(&coinbase(short), height, reward, true, p), Ok(false));
}

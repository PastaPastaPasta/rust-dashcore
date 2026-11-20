//! S1c F2's hard gate (PLAN-LK1b §6 S1c): [`NodeQuorumView`] on a real V. NodeSut's V takes
//! S1e's rotated capture run (`tests/data/lk1b-is/rot.json`, its blocks and sporks), and at each
//! of the run's three lock steps the view's answers are checked against the capture's record of
//! Core's quorums (`lq3r`):
//!
//! - the view's tip is the tip Core had when the lock arrived;
//! - the lock's cycle hash resolves to the cycle's height, and the lock verifies through the view;
//! - each captured quorum mined by the tip is found by hash (`QuorumScanView::quorum`, Core's
//!   `GetQuorum`), verified, with its base height, index and key;
//! - every scan start up to the tip answers, with Core's rotation predicate
//!   (`llmq/options.cpp:50-62`): regtest's `DIP0024Height` is 1, so a rotated type's first DKG
//!   cycle (based at 0, no block before it) holds starts before DIP0024 is active, and scans there
//!   are not rotated;
//! - each start whose scan the capture fully determines (Core's store-block rule over the newest
//!   quorums, as `dashd-locks`' capture view, `llmq/quorumsman.cpp:179-207`) gives that scan;
//! - a start's scan does not change as the chain grows past it.
//!
//! At the run's last tip, the view's tip scan is Core's `quorum list`.
#![cfg(feature = "test-support")]

#[allow(dead_code)]
mod lk1b_is;

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::primitives::decode_block;
use dash_evo::llmq::ScanEntry;
use dash_evo::locks::{IslockSchemes, QuorumScanView, QuorumStatus, ScanLookup, verify_islock};
use dash_evo::spork::SporkId;
use dash_params::{ChainSetup, LlmqType};
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_locks::is::LockedTx;
use dashd_locks::is::verify::NodeQuorumView;
use dashd_node::logging::Logger;
use dashd_node::sut::NodeSut;
use dashd_node::ui::Ui;
use lk1b_is::{Op, Step};
use serde_json::Value;

fn raw_fixture(run: &str) -> Value {
    let path = lk1b_is::data_dir().join(format!("{run}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("the fixture")).expect("JSON")
}

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "hex {s}");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// An RPC hash (display order) as wire bytes.
fn rpc_hash(s: &str) -> [u8; 32] {
    let mut bytes: [u8; 32] = unhex(s).try_into().expect("32 bytes");
    bytes.reverse();
    bytes
}

fn int(v: &Value) -> u32 {
    u32::try_from(v.as_u64().expect("an integer")).expect("small")
}

/// One captured quorum (`lq3r.quorums`).
#[derive(Clone, Debug)]
struct CapturedQuorum {
    hash: [u8; 32],
    height: u32,
    index: i16,
    key: Vec<u8>,
}

/// The capture's record of Core's quorums: for each type, its newest quorums at the run's last
/// tip, so every quorum newer than the oldest of them too.
struct Captured {
    quorums: BTreeMap<u8, Vec<CapturedQuorum>>,
    /// `quorum list` at the run's last tip, by type.
    list: BTreeMap<u8, Vec<[u8; 32]>>,
    tip: u32,
    scheme: BlsSchemeKind,
}

impl Captured {
    fn new(params: &ChainSetup, lq3r: &Value) -> Captured {
        let mut quorums = BTreeMap::new();
        for (t, list) in lq3r["quorums"].as_object().expect("quorums") {
            let list = list.as_array().expect("a list").iter().map(|q| CapturedQuorum {
                hash: rpc_hash(q["hash"].as_str().expect("a hash")),
                height: int(&q["height"]),
                index: i16::try_from(q["index"].as_i64().expect("an index")).expect("small"),
                key: unhex(q["public_key"].as_str().expect("a key")),
            });
            quorums.insert(t.parse().expect("a type"), list.collect());
        }
        let mut list = BTreeMap::new();
        for (name, hashes) in lq3r["quorum_list"].as_object().expect("quorum_list") {
            let llmq = params.consensus.llmq_params().find(|(t, _)| t.to_string() == *name);
            let t = llmq.unwrap_or_else(|| panic!("LLMQ {name}")).0.to_base();
            let hashes = hashes.as_array().expect("hashes");
            list.insert(t, hashes.iter().map(|h| rpc_hash(h.as_str().expect("a hash"))).collect());
        }
        let scheme = match lq3r["bls_scheme"].as_str() {
            Some("legacy") => BlsSchemeKind::Legacy,
            Some("basic") => BlsSchemeKind::Basic,
            other => panic!("bls_scheme {other:?}"),
        };
        Captured {
            quorums,
            list,
            tip: int(&lq3r["tip_height"]),
            scheme,
        }
    }

    /// Core's `ScanQuorums(type, start, signingActiveQuorumCount)` over the captured quorums,
    /// where the capture determines it: `None` when the store block lies inside a cycle's mining
    /// window (the capture does not say which of its quorums were mined yet), before the first
    /// cycle's window, or when the scan would read below the oldest captured quorum.
    fn scan(&self, params: &ChainSetup, t: LlmqType, start: u32) -> Option<Vec<[u8; 32]>> {
        let llmq = params.consensus.llmq(t).expect("configured");
        let count = usize::from(llmq.signing_active_quorum_count);
        let interval = u32::from(llmq.dkg_interval);
        let (ws, we) =
            (u32::from(llmq.dkg_mining_window_start), u32::from(llmq.dkg_mining_window_end));
        let cycle = start - start % interval;
        let store = if start < cycle + ws {
            cycle.checked_sub(interval - we)?
        } else if start > cycle + we {
            cycle + we
        } else {
            start
        };
        let mut cycles: BTreeMap<u32, Vec<&CapturedQuorum>> = BTreeMap::new();
        for q in &self.quorums[&t.to_base()] {
            cycles.entry(q.height - q.height % interval).or_default().push(q);
        }
        let mut out = Vec::new();
        for (base, mut set) in cycles.into_iter().rev() {
            if store < base + ws {
                continue;
            }
            if store < base + we {
                return None;
            }
            set.sort_by_key(|q| q.index);
            out.extend(set.into_iter().map(|q| q.hash));
            if out.len() >= count {
                out.truncate(count);
                return Some(out);
            }
        }
        None
    }
}

/// A scan as the view answered it: the rotation predicate and the quorum hashes, newest first.
type Scan = (bool, Vec<[u8; 32]>);

/// A quorum the view found by hash: its base height, index and key bytes.
type Found = Option<(u32, i16, Vec<u8>)>;

/// What the view answered in one V turn.
#[derive(Debug)]
struct Answers {
    tip: u32,
    /// `(type, start)` → (rotation, the scan's quorum hashes), for every start up to the tip.
    scans: BTreeMap<(u8, u32), Scan>,
    /// `(type, hash)` → (base height, key bytes) for each captured quorum.
    quorums: BTreeMap<(u8, [u8; 32]), Found>,
    /// The lock's cycle hash's height.
    cycle_height: Option<u32>,
    /// The lock through the view: the verifying quorum's hash, or the error.
    verified: Result<[u8; 32], String>,
    /// The tip scan per type, as long as the captured `quorum list`.
    tip_scans: BTreeMap<u8, Vec<[u8; 32]>>,
}

fn hashes(lookup: ScanLookup) -> Scan {
    let ScanLookup::Held(scan) = lookup else {
        panic!("V holds every scan: {lookup:?}");
    };
    (scan.rotation_enabled, scan.entries.iter().map(|e| *e.quorum_hash().as_bytes()).collect())
}

fn answers(sut: &NodeSut, captured: &Captured, lock: LockedTx) -> Answers {
    let params = sut.params().clone();
    let all: std::collections::BTreeSet<u8> =
        captured.quorums.keys().chain(captured.list.keys()).copied().collect();
    let types: Vec<(LlmqType, Vec<[u8; 32]>, usize)> = all
        .into_iter()
        .map(|t| {
            let listed = captured.list.get(&t).map_or(0, Vec::len);
            let hashes = captured.quorums.get(&t).into_iter().flatten().map(|q| q.hash).collect();
            (LlmqType::from_base(t), hashes, listed)
        })
        .collect();
    let scheme = captured.scheme;
    let chain = sut.chain().expect("V runs");
    let reply = chain.on_v(move |o, q| {
        let view = NodeQuorumView::new(o, q, &params).expect("the view");
        let binding = q.bind(o);
        let reader = q.chain.reader();
        let tip = view.tip_height();
        let cycle = BlockHash::from(*lock.cycle_hash().as_bytes());
        let verified = verify_islock(&params, &view, IslockSchemes::both(view.bls()), &lock);
        let mut out = Answers {
            tip,
            scans: BTreeMap::new(),
            quorums: BTreeMap::new(),
            cycle_height: view.block_height(&cycle).expect("a header read"),
            verified: verified
                .map(|v| *v.quorum.quorum_hash().as_bytes())
                .map_err(|e| format!("{e:?}")),
            tip_scans: BTreeMap::new(),
        };
        for (t, captured, listed) in &types {
            for start in 0..=tip {
                let scan = hashes(view.scan(*t, start).expect("a scan"));
                out.scans.insert((t.to_base(), start), scan);
            }
            for hash in captured {
                let hash_q = QuorumHash::from(*hash);
                let found = match view.quorum(*t, &hash_q).expect("a quorum read") {
                    None => None,
                    Some(QuorumStatus::Verified(vq)) => {
                        let node = binding.quorum(*t, &hash_q).expect("a quorum read");
                        let base = node.expect("V holds the quorum the view names").base;
                        let height = reader.entry(base).expect("the base's header").height;
                        let key = vq.public_key().to_bytes(scheme).to_vec();
                        Some((height, vq.quorum_index(), key))
                    }
                    Some(other) => panic!("V's mined quorum {hash:?} is {other:?}"),
                };
                out.quorums.insert((t.to_base(), *hash), found);
            }
            let at_tip = hashes(view.scan_at_tip(*t, *listed).expect("a tip scan"));
            out.tip_scans.insert(t.to_base(), at_tip.1);
        }
        out
    });
    reply.recv().expect("V answers")
}

/// One op of the run on the SUT: sporks and blocks, the view's inputs. The caller checks a lock;
/// ChainLocks and restarts are the differential's.
fn apply(sut: &NodeSut, step: &Step) {
    match &step.op {
        Op::Spork {
            id,
            value,
        } => {
            let id = SporkId(i32::try_from(*id).expect("a spork id"));
            let signed = sut.sporks().expect("loaded").update_spork(id, *value);
            assert!(signed.is_some(), "step {}: the spork key signs", step.step);
        }
        Op::Block {
            bytes,
            ..
        } => {
            let block = decode_block(&mut &bytes[..]).expect("a block").into_value();
            let hash = block.hash();
            let chain = sut.chain().expect("V runs");
            let r = chain.process_block(Arc::new(block), true, None, true).recv().expect("V");
            assert!(r.accepted && r.state.is_valid(), "step {}: {r:?}", step.step);
            sut.settle().expect("V settles");
            let best = sut.rpc("getbestblockhash", "[]").expect("loaded").expect("the tip");
            if best != format!("\"{hash}\"") {
                let tips = sut.rpc("getchaintips", "[]");
                panic!("step {}: {hash} does not connect: tip {best}, tips {tips:?}", step.step);
            }
        }
        Op::IsLock {
            ..
        }
        | Op::ClRecord {
            ..
        }
        | Op::Restart {
            ..
        } => {}
    }
}

#[test]
fn node_quorum_view_on_the_rotated_capture_run_answers_as_core() {
    let raw = raw_fixture("rot");
    let fixture = lk1b_is::capture("rot");
    let dir = tempfile::tempdir().unwrap();
    lk1b_is::node_datadir(dir.path(), "rot");
    let argv = lk1b_is::node_args("rot");
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
    let sut = NodeSut::new(dir.path(), &argv, lk1b_is::clock(None, &fixture.steps[0]), ui)
        .expect("the SUT starts");
    let params = sut.params().clone();
    assert_eq!(params.consensus.dip0024_height.to_u32(), 1, "regtest's DIP0024Height");
    let captured = Captured::new(&params, &raw["lq3r"]);
    let rotated = params.consensus.llmq_type_dip0024_instant_send.to_base();
    assert!(captured.quorums.contains_key(&rotated), "the run has the rotated type's quorums");
    let locks: BTreeMap<usize, &Value> = raw["lq3r"]["locks"]
        .as_array()
        .expect("locks")
        .iter()
        .map(|l| (int(&l["step"]) as usize, l))
        .collect();

    let mut now = None;
    let mut history: BTreeMap<(u8, u32), Scan> = BTreeMap::new();
    let (mut checked, mut referenced, mut pre_activation) = (0, 0, 0);
    let mut last_lock = None;
    for step in &fixture.steps {
        let t = lk1b_is::clock(now, step);
        now = Some(t);
        sut.clock().set_mocktime(t);
        apply(&sut, step);
        let Op::IsLock {
            bytes,
            ..
        } = &step.op
        else {
            continue;
        };
        let want = locks.get(&step.step).unwrap_or_else(|| panic!("step {} is a lock", step.step));
        let lock = LockedTx::decode(&mut &bytes[..]).expect("an isdlock");
        last_lock = Some(lock.clone());
        let got = answers(&sut, &captured, lock);
        let at = format!("lock {} (step {})", want["name"], step.step);

        assert_eq!(got.tip, int(&want["signed_at_tip"]), "{at}: the view's tip");
        assert_eq!(got.cycle_height, Some(int(&want["cycle_height"])), "{at}: the cycle's height");
        let signer = got.verified.unwrap_or_else(|e| panic!("{at}: the lock verifies: {e}"));
        let signers = &captured.quorums[&rotated];
        assert!(signers.iter().any(|q| q.hash == signer), "{at}: signed by a captured quorum");

        for (&t, qs) in &captured.quorums {
            let llmq = params.consensus.llmq(LlmqType::from_base(t)).expect("configured");
            let interval = u32::from(llmq.dkg_interval);
            for start in 0..=got.tip {
                let (rotation, scan) = &got.scans[&(t, start)];
                // Core: llmq/options.cpp:50-62, with regtest's DIP0024Height of 1.
                let cycle_base = start - start % interval;
                assert_eq!(*rotation, llmq.use_rotation && cycle_base >= 1, "{at}: {t} at {start}");
                if llmq.use_rotation && cycle_base == 0 {
                    pre_activation += 1;
                }
                if let Some(want) = captured.scan(&params, LlmqType::from_base(t), start) {
                    assert_eq!(scan, &want, "{at}: type {t}'s scan at {start}");
                    referenced += 1;
                }
                let now = (*rotation, scan.clone());
                if let Some(before) = history.insert((t, start), now.clone()) {
                    assert_eq!(before, now, "{at}: type {t}'s scan at {start} moved");
                }
            }
            for q in qs {
                let found = &got.quorums[&(t, q.hash)];
                if q.height + u32::from(llmq.dkg_mining_window_end) <= got.tip {
                    let want = Some((q.height, q.index, q.key.clone()));
                    assert_eq!(found, &want, "{at}: type {t}'s quorum at {}", q.height);
                } else if q.height > got.tip {
                    assert_eq!(found, &None, "{at}: type {t}'s quorum at {} is not yet", q.height);
                }
            }
        }
        checked += 1;
    }
    assert_eq!(checked, 3, "locks A, B and D");
    assert!(referenced > 0 && pre_activation > 0, "{referenced} referenced, {pre_activation} pre");

    let got = answers(&sut, &captured, last_lock.expect("lock D"));
    assert_eq!(got.tip, captured.tip, "the run's last tip");
    for (t, list) in &captured.list {
        assert_eq!(&got.tip_scans[t], list, "type {t}: Core's quorum list at the last tip");
    }
}

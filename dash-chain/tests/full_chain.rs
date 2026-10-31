//! A-4 (SB4 §15.1): H-M and H-T accepted from genesis, unwindowed (node) and windowed (SPV at
//! w = 10,000 and w = 1,000), with the reference node's stale branches, then the active chain
//! moved to the dump's tip. `chain_tips` and the tips' chain work equal what the reference node
//! knows (`tests/data/headers/tips-<net>.json`, recorded by `fetch_tips.py`) under §10.2's
//! projection: `active` exactly, every other status as `header-valid`, and SB4 must report every
//! non-active tip as `headers-only`.
//!
//! The reference node has moved past the dump, so the fixture holds its stale branches mapped
//! onto the dump's heights, and the dump tip's chain work. The dump's tip is the most-work header
//! chain, and both replays end with it active: a node connected along it, SPV's stage 4.
//!
//! `#[ignore]`d (nightly): `tests/data/headers/fetch.sh`, then
//! `cargo test --release -p dash-chain --features test-util --test full_chain -- --ignored`.

#![cfg(not(loom))]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::FromStr as _;

use dash_chain::{
    BlockId, ChainConfig, ChainTip, ChainTipStatus, Evicting, HeaderBatch, HeaderChain, HeaderStop,
    HeadersResult, MemHeaderStore, NoStore, Permanent, Work,
};
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use serde_json::Value;
use support::headers::{self, BATCH, accepted, batches, decode};

/// A stale branch of the reference node, cut at the dump's height (`fetch_tips.py`).
struct Branch {
    hash: BlockHash,
    height: u32,
    /// The dump header it forks from.
    fork: u32,
    work: Work,
    /// Parent first.
    headers: Vec<String>,
}

struct Fixture {
    dump_height: u32,
    dump_work: Work,
    stale: Vec<Branch>,
}

fn hash(v: &Value) -> BlockHash {
    BlockHash::from_str(v.as_str().expect("a hash")).expect("hex")
}

fn work(v: &Value) -> Work {
    v.as_str().expect("chain work").parse().expect("hex")
}

fn height(v: &Value) -> u32 {
    u32::try_from(v.as_u64().expect("a height")).expect("u32")
}

fn fixture(net: &str, dump: &[BlockHash]) -> Fixture {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/data/headers/tips-{net}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let raw: Value = serde_json::from_str(&text).expect("JSON");
    let d = height(&raw["dump_height"]);
    assert_eq!(dump.len(), d as usize + 1, "the fixture's dump");
    assert_eq!(hash(&raw["dump_tip"]["hash"]), dump[d as usize]);
    let stale = raw["stale"]
        .as_array()
        .expect("stale")
        .iter()
        .map(|b| Branch {
            hash: hash(&b["hash"]),
            height: height(&b["height"]),
            fork: height(&b["fork_height"]),
            work: work(&b["chainwork"]),
            headers: b["headers"]
                .as_array()
                .expect("headers")
                .iter()
                .map(|x| x.as_str().expect("hex").to_owned())
                .collect(),
        })
        .collect();
    Fixture {
        dump_height: d,
        dump_work: work(&raw["dump_tip"]["chainwork"]),
        stale,
    }
}

/// Accepts every stale branch in batches of at most [`BATCH`] through `accept`, and returns
/// the tips of those refused as `bad-fork-prior-to-checkpoint`: the replay holds the dump's
/// checkpoints before it sees a branch the reference node took before it had them, which Core
/// would refuse too. Only a branch forking below the last checkpoint at or below the dump may be
/// refused, and only at its first header.
fn accept_stale(
    fx: &Fixture,
    p: &ChainSetup,
    mut accept: impl FnMut(HeaderBatch) -> HeadersResult,
) -> BTreeSet<BlockHash> {
    let last_checkpoint = p
        .checkpoints
        .iter()
        .map(|c| c.0.to_u32())
        .filter(|&h| h <= fx.dump_height)
        .max()
        .unwrap_or(0);
    let mut refused = BTreeSet::new();
    for branch in &fx.stale {
        let all: Vec<_> = branch.headers.iter().map(|h| decode(&support::hex(h))).collect();
        for (k, chunk) in all.chunks(BATCH).enumerate() {
            let r = accept(headers::batch(chunk.iter().copied()));
            if let Some(HeaderStop::Invalid {
                index,
                reject,
            }) = &r.stop
                && reject.reject_reason() == "bad-fork-prior-to-checkpoint"
            {
                assert!(
                    k == 0 && *index == 0 && branch.fork < last_checkpoint,
                    "{}: refused at header {} of batch {k}, fork {}, last checkpoint {last_checkpoint}",
                    branch.hash,
                    index,
                    branch.fork,
                );
                refused.insert(branch.hash);
                break;
            }
            accepted(&r);
        }
    }
    refused
}

type Tips = BTreeSet<(BlockHash, u32, u32, &'static str)>;

/// The tips a replay with the dump's tip active must report, and their chain work.
fn expected(
    fx: &Fixture,
    dump: &[BlockHash],
    refused: &BTreeSet<BlockHash>,
) -> (Tips, BTreeMap<BlockHash, Work>) {
    let (d, tip) = (fx.dump_height, dump[fx.dump_height as usize]);
    let mut tips = Tips::from([(tip, d, 0, "active")]);
    let mut works = BTreeMap::from([(tip, fx.dump_work)]);
    for b in fx.stale.iter().filter(|b| !refused.contains(&b.hash)) {
        // Equal work stays behind the dump, which the replay accepted first, as Core keeps the
        // first-seen chain.
        assert!(b.work <= fx.dump_work, "the dump is the most-work header chain");
        tips.insert((b.hash, b.height, b.height - b.fork, "header-valid"));
        works.insert(b.hash, b.work);
    }
    (tips, works)
}

/// §10.2's A-4 projection of SB4's answer.
fn projected(tips: &[ChainTip]) -> (Tips, BTreeMap<BlockHash, Work>) {
    let status = |t: &ChainTip| match t.status {
        ChainTipStatus::Active => "active",
        ChainTipStatus::HeadersOnly => "header-valid",
        s => panic!("{}: a header-only replay reports {}", t.hash, s.as_str()),
    };
    let set = tips.iter().map(|t| (t.hash, t.height, t.branchlen, status(t))).collect();
    (set, tips.iter().map(|t| (t.hash, t.chain_work)).collect())
}

/// The node: everything accepted, then `connect_tip` along the dump to its tip.
fn node(net: &str) {
    let p = headers::params(net);
    let mut chain = HeaderChain::<NoStore, Permanent>::new_genesis(
        p.clone(),
        ChainConfig::node(&p),
        headers::services(&p),
        NoStore,
    )
    .expect("genesis");
    chain.init_active_tip(BlockId(0)).expect("Core LoadChainTip");
    let mut dump = Vec::new();
    for b in batches(net) {
        dump.extend_from_slice(&b.hashes);
        accepted(&chain.accept(b, None).expect("accept"));
    }
    let fx = fixture(net, &dump);
    let refused = accept_stale(&fx, &p, |b| chain.accept(b, None).expect("accept"));
    let reader = chain.reader();
    let mut hold = chain.hold_tip();
    for h in &dump[1..] {
        hold.connect_tip(reader.lookup(h).expect("accepted")).expect("connects");
    }
    hold.release();

    let (want, works) = expected(&fx, &dump, &refused);
    let (got, got_works) = projected(&chain.chain_tips().expect("chain_tips"));
    assert_eq!(got, want, "{net}: getchaintips");
    assert_eq!(got_works, works, "{net}: chain work");
}

/// SPV at window `w`: one §9.1 step per batch (accept, stage 3 into a `MemHeaderStore`, stage 4 to
/// the best header, which evicts, stage 5), the stale branches last.
fn spv(net: &str, w: u32) {
    let p = headers::params(net);
    let mut store = headers::genesis_store(&p);
    let mut chain = HeaderChain::<MemHeaderStore, Evicting>::new_genesis(
        p.clone(),
        ChainConfig::spv(&p, w),
        headers::services(&p),
        store.clone(),
    )
    .expect("genesis");
    chain.commit_mem_step(&mut store).expect("commit");
    chain.records_committed();
    let mut step = |chain: &mut HeaderChain<MemHeaderStore, Evicting>, b| {
        let r = chain.accept(b, None).expect("accept");
        chain.commit_mem_step(&mut store).expect("commit");
        let to = chain.best_header();
        if chain.private_tip().expect("a tip").0.id != to {
            chain.tip_plan(to).expect("a plan");
            chain.set_active_tip(to).expect("stage 4");
        }
        chain.records_committed();
        r
    };
    let mut dump = Vec::new();
    for b in batches(net) {
        dump.extend_from_slice(&b.hashes);
        accepted(&step(&mut chain, b));
    }
    let fx = fixture(net, &dump);
    let refused = accept_stale(&fx, &p, |b| step(&mut chain, b));
    assert_eq!(chain.private_tip().expect("a tip").0.hash, dump[fx.dump_height as usize]);

    let (want, works) = expected(&fx, &dump, &refused);
    let (got, got_works) = projected(&chain.chain_tips().expect("chain_tips"));
    assert_eq!(got, want, "{net} w = {w}: getchaintips");
    assert_eq!(got_works, works, "{net} w = {w}: chain work");
}

#[test]
#[ignore = "nightly: needs H-M (tests/data/headers/fetch.sh)"]
fn full_chain_mainnet_node() {
    node("mainnet");
}

#[test]
#[ignore = "nightly: needs H-M (tests/data/headers/fetch.sh)"]
fn full_chain_mainnet_spv_w10000() {
    spv("mainnet", 10_000);
}

#[test]
#[ignore = "nightly: needs H-M (tests/data/headers/fetch.sh)"]
fn full_chain_mainnet_spv_w1000() {
    spv("mainnet", 1_000);
}

#[test]
#[ignore = "nightly: needs H-T (tests/data/headers/fetch.sh)"]
fn full_chain_testnet_node() {
    node("testnet");
}

#[test]
#[ignore = "nightly: needs H-T (tests/data/headers/fetch.sh)"]
fn full_chain_testnet_spv_w10000() {
    spv("testnet", 10_000);
}

#[test]
#[ignore = "nightly: needs H-T (tests/data/headers/fetch.sh)"]
fn full_chain_testnet_spv_w1000() {
    spv("testnet", 1_000);
}

//! Steps 1–7 over a private testnet Core node's own replies (`tests/data/sb11-testnet-v24.0.captures.json.gz`,
//! provenance in its `PROVENANCE.md`): the headers from a floor 5000 blocks below the tip, Core's diff from
//! genesis 1300 blocks below it and every step diff after, `qrinfo`s for the last three rotation cycles,
//! and the ChainLocks the last blocks' CbTx carry, each verified by Core; and the same node's one diff
//! for each fetch item 12's restart asks for (`sb11-testnet-fetch-v24.0.captures.json.gz`).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use dash_chain::{
    AncestryError, AnchorPack, AnchoredAncestry, BlockRef, ChainConfig, Evicting, HeaderChain,
    NoStore, Work,
};
use dash_consensus::BlockId;
use dash_consensus::ctx::HeaderCtx;
use dash_consensus::primitives::{Header, decode_header};
use dash_evo::llmq::{SIGN_HEIGHT_OFFSET, WORK_DIFF_DEPTH};
use dash_evo::locks::LockReject;
use dash_num::{Arith256, CompactTarget};
use dash_p2p_core::{ClSig, MnListDiff, QrInfo};
use dash_params::{BuriedDeployment, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, BlockHeader, LlmqType, MerkleRoot, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use serde_json::Value;

use super::{Capture, decode, hex, load, rpc, u32_of};
#[cfg(feature = "testnet-replay")]
use super::{asked_heights, step_diff_of};
use crate::{
    Coverage, LlmqConfig, LockOutcome, Lookup, MasternodeListEngine, MnChain, MnEvent, MnRequest,
    Mode, QuorumState, RequestKey, SetAt, Unavailable,
};

fn testnet() -> Capture {
    static FIXTURE: OnceLock<Value> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        load(include_bytes!("../../tests/data/sb11-testnet-v24.0.captures.json.gz"))
    })
}

/// Core's one `mnlistdiff` for each (base, target) item 12's restart asks for
/// (`tests/data/sb11-testnet-fetch-v24.0.captures.json.gz`, from the same node as [`testnet`]; D55).
#[cfg(feature = "testnet-replay")]
fn testnet_fetch() -> Capture {
    static FIXTURE: OnceLock<Value> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        load(include_bytes!("../../tests/data/sb11-testnet-fetch-v24.0.captures.json.gz"))
    })
}

/// Core's reply to `getmnlistd(base, target)` (heights; `None` a null base): the fetch capture's,
/// or `f`'s step diff or diff from genesis when the request is exactly theirs.
#[cfg(feature = "testnet-replay")]
fn cores_reply(f: Capture, base: Option<u32>, target: u32) -> Option<&'static Value> {
    let is = |d: &&Value| {
        d["base_height"].as_u64().map(|b| b as u32) == base && u32_of(&d["height"]) == target
    };
    let fetched = testnet_fetch()["diffs"].as_array().expect("diffs").iter();
    let captured =
        ["step", "from_genesis"].into_iter().flat_map(|k| f["diffs"][k].as_array().expect("diffs"));
    fetched.chain(captured).find(is).map(|d| &d["payload"])
}

/// Four blocks above a type 1 and 6 cycle base below the diff from genesis (1573233): that base
/// is known, its work block is not.
const FLOOR_ABOVE_A_BASE: u32 = 1_573_196;

fn config() -> LlmqConfig {
    let params =
        ChainParamsBuilder::new(NetworkKind::Testnet, Release::V24_0).build().expect("params");
    LlmqConfig::new(Arc::new(params)).expect("in the domain")
}

/// The capture's headers from its floor, height as id.
struct TestnetChain {
    floor: u32,
    headers: Vec<HeaderCtx>,
    roots: Vec<MerkleRoot>,
    ids: HashMap<BlockHash, u32>,
}

impl TestnetChain {
    fn new(f: Capture) -> Self {
        let floor = u32_of(&f["floor"]);
        let raw: Vec<_> = f["headers"]
            .as_array()
            .expect("headers")
            .iter()
            .map(|h| {
                let bytes = hex(h.as_str().expect("a header"));
                decode_header(&mut &bytes[..]).expect("a header").into_value()
            })
            .collect();
        let mut headers: Vec<HeaderCtx> = Vec::new();
        let mut times = Vec::new();
        for (height, header) in (floor..).zip(&raw) {
            let h = header.raw();
            if let Some(prev) = headers.last() {
                assert_eq!(h.prev_hash, prev.hash, "the headers chain");
            }
            times.push(i64::from(h.time));
            let mut window: Vec<i64> = times.iter().rev().take(11).copied().collect();
            window.sort_unstable();
            headers.push(HeaderCtx {
                height,
                hash: header.hash(),
                time: h.time,
                bits: CompactTarget::new(h.bits),
                version: h.version,
                mtp: window[window.len() / 2],
                chain_work: Arith256::ZERO,
            });
        }
        let hashes = f["block_hashes"].as_array().expect("block_hashes");
        for (h, hash) in headers.iter().zip(hashes) {
            assert_eq!(h.hash, BlockHash::from(rpc(hash.as_str().expect("a hash"))));
        }
        TestnetChain {
            floor,
            roots: raw.iter().map(|h| h.raw().merkle_root).collect(),
            ids: headers.iter().map(|h| (h.hash, h.height)).collect(),
            headers,
        }
    }

    /// The same chain between `floor` and `tip`: an anchor higher up, headers not yet synced.
    fn window(&self, floor: u32, tip: u32) -> TestnetChain {
        let (from, to) = ((floor - self.floor) as usize, (tip - self.floor) as usize + 1);
        TestnetChain {
            floor,
            headers: self.headers[from..to].to_vec(),
            roots: self.roots[from..to].to_vec(),
            ids: self
                .ids
                .iter()
                .filter(|(_, h)| (floor..=tip).contains(*h))
                .map(|(k, v)| (*k, *v))
                .collect(),
        }
    }

    fn tip(&self) -> u32 {
        self.floor + u32::try_from(self.headers.len()).expect("small") - 1
    }

    fn hash(&self, height: u32) -> BlockHash {
        self.headers[(height - self.floor) as usize].hash
    }

    fn index(&self, height: u32) -> Result<usize, AncestryError> {
        if height < self.floor {
            return Err(AncestryError::BelowFloor {
                height,
            });
        }
        assert!(height <= self.tip(), "an id the chain never issued");
        Ok((height - self.floor) as usize)
    }
}

impl AnchoredAncestry for TestnetChain {
    fn floor(&self) -> u32 {
        self.floor
    }

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError> {
        Ok(self.headers[self.index(id.0)?])
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError> {
        if height > id.0 {
            return Ok(None);
        }
        self.index(height).map(|_| Some(BlockId(height)))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError> {
        match id.0.checked_sub(1) {
            None => Ok(None),
            Some(h) => self.index(h).map(|_| Some(BlockId(h))),
        }
    }
}

impl MnChain for TestnetChain {
    fn ancestry(&self) -> &dyn AnchoredAncestry {
        self
    }

    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        Ok(self.ids.get(hash).map(|h| BlockId(*h)))
    }

    fn active_tip(&self) -> Result<BlockRef, AncestryError> {
        let tip = self.tip();
        Ok(BlockRef {
            id: BlockId(tip),
            hash: self.hash(tip),
            height: tip,
        })
    }

    fn merkle_root(&self, id: BlockId) -> Result<Option<MerkleRoot>, AncestryError> {
        Ok(Some(self.roots[self.index(id.0)?]))
    }
}

/// A captured ChainLock as its `clsig` bytes: height, block hash, signature.
fn clsig(c: &Value) -> ClSig {
    let mut bytes = i32::try_from(u32_of(&c["height"])).expect("a height").to_le_bytes().to_vec();
    bytes.extend(rpc(c["block_hash"].as_str().expect("a hash")));
    bytes.extend(hex(c["signature"].as_str().expect("a signature")));
    let mut r = &bytes[..];
    let cl = ClSig::decode(&mut r).expect("a clsig");
    assert!(r.is_empty());
    cl
}

fn apply_qrinfos(
    engine: &mut MasternodeListEngine,
    f: Capture,
    chain: &TestnetChain,
    cfg: &LlmqConfig,
) {
    for q in f["qrinfos"].as_array().expect("qrinfos") {
        let reply: QrInfo = decode(&q["payload"]);
        engine.apply_qrinfo(&reply, chain, cfg).unwrap_or_else(|e| {
            panic!("{} extra share {}: {e}", q["request_height"], q["extra_share"])
        });
    }
}

/// Core's step diffs to the heights above `from`, in order.
fn apply_steps(
    engine: &mut MasternodeListEngine,
    f: Capture,
    chain: &TestnetChain,
    cfg: &LlmqConfig,
    from: u32,
) {
    apply_steps_to(engine, f, chain, cfg, from, u32::MAX);
}

/// Core's step diffs to the heights in (`from`, `to`], in order.
fn apply_steps_to(
    engine: &mut MasternodeListEngine,
    f: Capture,
    chain: &TestnetChain,
    cfg: &LlmqConfig,
    from: u32,
    to: u32,
) {
    for d in f["diffs"]["step"].as_array().expect("steps") {
        let h = u32_of(&d["height"]);
        if h > from && h <= to {
            engine
                .apply_diff(&decode(&d["payload"]), chain, cfg)
                .unwrap_or_else(|e| panic!("{h}: {e}"));
        }
    }
}

/// Core's step diffs to the heights in (`from`, `to`], every list pinned until the last applies
/// (a block step 5 asks for is not the host's base, D34, so it would go before the next step);
/// then only `pins` stay pinned and retention runs at the chain's tip.
#[cfg(feature = "testnet-replay")]
fn apply_steps_held(
    engine: &mut MasternodeListEngine,
    f: Capture,
    chain: &TestnetChain,
    cfg: &LlmqConfig,
    (from, to): (u32, u32),
    pins: &[u32],
) {
    for d in f["diffs"]["step"].as_array().expect("steps") {
        let h = u32_of(&d["height"]);
        if h > from && h <= to {
            engine.pin_list(BlockId(h));
            engine
                .apply_diff(&decode(&d["payload"]), chain, cfg)
                .unwrap_or_else(|e| panic!("{h}: {e}"));
        }
    }
    engine.unpin_lists();
    for at in pins {
        engine.pin_list(BlockId(*at));
    }
    engine.tip_changed(chain, cfg).expect("retention");
}

/// The store block of the ChainLock at 1574501's scan: the type 1 mining window's end.
const STORE: u32 = 1_574_490;

/// The newest `qrinfo`'s H work block.
fn newest_work_block(f: Capture, chain: &TestnetChain) -> u32 {
    let newest = f["qrinfos"]
        .as_array()
        .expect("qrinfos")
        .iter()
        .max_by_key(|q| u32_of(&q["request_height"]))
        .expect("a qrinfo");
    let reply: QrInfo = decode(&newest["payload"]);
    chain.lookup(&reply.diff_h.block_hash).expect("ok").expect("H's work block").0
}

/// Each quorum Core lists at the tip, with its type, height and state.
fn quorum_states(
    engine: &MasternodeListEngine,
    f: Capture,
) -> Vec<(LlmqType, u32, Option<QuorumState>)> {
    let mut states = Vec::new();
    for (t, quorums) in f["quorums"].as_object().expect("quorums") {
        let llmq = LlmqType::from_base(t.parse().expect("a type"));
        for q in quorums.as_array().expect("a type's quorums") {
            let hash = QuorumHash::from(rpc(q["quorumHash"].as_str().expect("a hash")));
            states.push((llmq, u32_of(&q["height"]), engine.quorum(llmq, &hash).map(|(_, s)| s)));
        }
    }
    states
}

/// SB0 §5.3 R6-M3 after pruning and a restart, over Core's bytes (PLAN SB11 row item 12, D52,
/// D54, D55). Core's diffs mine the type 6 quorum based at 1,573,608 and replace it at
/// r = 1,574,195. The engine syncs every list; with the lists D29 keeps for the 288-block type's
/// cycles dropped, as retention drops them once those windows close, the tip prunes its row
/// (r < tip − R), and the engine restarts. Every fetch is answered with Core's own reply to that
/// request, `getmnlistd(base, target)`, from the same node as the capture. At r + W_P − 1 the
/// lookup asks for its lower target, r − 1 (the upper one is within the horizon, held), from the
/// newest list held below, 1,573,336. Core's one diff holds r − 1's list only, so step 5 cannot
/// verify the quorum yet and asks for its work block, 1,573,600, from the same base; Core's diff to
/// it verifies the quorum with the key it had before pruning, and the set at r − 1 holds it: found.
/// At r + W_P the lower target, r, does not: not found. About 2,700 diffs unoptimised, so it runs
/// in the gate's feature step.
#[cfg(feature = "testnet-replay")]
#[test]
fn a_pruned_replacement_verifies_from_cores_diffs_after_a_restart() {
    const BASE: u32 = 1_573_608;
    const REPLACED: u32 = 1_574_195;
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let platform = cfg.params().consensus.llmq_type_platform;
    let genesis = &f["diffs"]["from_genesis"][0];
    let start = u32_of(&genesis["height"]);
    let replacing = step_diff_of(f, REPLACED);
    let hash = QuorumHash::from(*chain.hash(BASE).as_bytes());
    assert!(
        replacing
            .deleted_quorums
            .iter()
            .any(|q| q.llmq_type == platform && q.hash == chain.hash(BASE)),
        "Core's diff to r deletes it"
    );
    assert!(REPLACED + cfg.r() < chain.tip() && REPLACED + cfg.w_p() <= chain.tip());

    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&decode(&genesis["payload"]), &chain, &cfg).expect("Core's diff");
    apply_steps(&mut engine, f, &chain, &cfg, start);
    let (qc, verified) = engine.quorum(platform, &hash).expect("its row");
    let QuorumState::Verified(before) = verified.clone() else {
        panic!("{verified:?}")
    };
    engine.tip_changed(&chain, &cfg).expect("step 5");
    // D29 keeps the work blocks of the 288-block type's cycles below the horizon, and those since
    // the mine hold the quorum, so its row stays at the capture's tip; dropped as retention drops
    // them once the type's windows close, the tip prunes it.
    assert!(engine.holds_row(&qc));
    let holding: Vec<u32> =
        engine.held_heights().into_iter().filter(|h| (BASE..REPLACED).contains(h)).collect();
    assert!(!holding.is_empty());
    for h in holding {
        engine.forget_list(BlockId(h));
    }
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert!(!engine.holds_row(&qc), "pruned at tip − R");
    let mut engine = super::persist::reloaded(&mut engine, &chain);
    assert!(engine.quorum(platform, &hash).is_none());

    // Core's reply to the request: its base and target, by height.
    let answer = |engine: &mut MasternodeListEngine, key: RequestKey| {
        let (base, target) = engine
            .requests()
            .into_iter()
            .find_map(|r| match r {
                MnRequest::MnListDiff {
                    base,
                    target_height,
                    ..
                } if r.key() == key => Some((base, target_height)),
                _ => None,
            })
            .expect("asked");
        let base = base.map(|b| chain.lookup(&b).expect("ok").expect("on the chain").0);
        let payload = cores_reply(f, base, target)
            .unwrap_or_else(|| panic!("no captured reply for {base:?} to {target}"));
        engine.apply_diff(&decode(payload), &chain, &cfg).expect("Core's diff");
    };
    let key = |h: u32| RequestKey::MnListDiff(chain.hash(h));
    let lookup = |e: &MasternodeListEngine, h| e.lookup_quorum(platform, &hash, h, &chain, &cfg);
    // The commitments the replies bring back also ask for work blocks below the diff from
    // genesis, out of the capture's reach.
    let asked = |e: &MasternodeListEngine| -> Vec<u32> {
        asked_heights(e).into_iter().filter(|t| *t >= start).collect()
    };
    let fresh = |e: &MasternodeListEngine, out: &[u32]| -> Vec<u32> {
        asked(e).into_iter().filter(|t| !out.contains(t)).collect()
    };
    let work = BASE - WORK_DIFF_DEPTH;
    for (h, found) in [(REPLACED + cfg.w_p() - 1, true), (REPLACED + cfg.w_p(), false)] {
        let low = h - cfg.w_p();
        assert_eq!(lookup(&engine, h).expect("no fault"), Lookup::Fetch, "{h}");
        let out = asked(&engine);
        engine.fetch_lookup(platform, &hash, h, &chain, &cfg).expect("not busy");
        // The target at h is within the horizon, so held (D28).
        assert_eq!(fresh(&engine, &out), [low], "{h}");
        answer(&mut engine, key(low));
        if found {
            // Step 5's own request: Core's diff to r − 1 holds that list only.
            let state = engine.quorum(platform, &hash).expect("held").1;
            assert_eq!(state, QuorumState::Pending(key(work)), "{h}");
            assert_eq!(fresh(&engine, &out), [work], "{h}");
            answer(&mut engine, key(work));
        }
        // Nothing this lookup asked is out; the set at 1,573,600 brings the other type 6 quorums'
        // own step-5 requests, their work blocks every 24 blocks below it.
        let left = asked(&engine);
        assert!(!left.contains(&low) && !left.contains(&work), "{h}: {left:?}");
        assert!(
            left.iter().all(|t| (t + WORK_DIFF_DEPTH).is_multiple_of(24) && *t < work),
            "{h}: {left:?}"
        );
        engine.tip_changed(&chain, &cfg).expect("step 5");
        if found {
            assert_eq!(engine.quorum(platform, &hash).expect("held").1, verified, "{h}");
            match lookup(&engine, h).expect("no fault") {
                Lookup::Found(v) => assert_eq!(v, before, "{h}"),
                other => panic!("{h}: {other:?}"),
            }
        } else {
            assert_eq!(lookup(&engine, h).expect("no fault"), Lookup::NotFound, "{h}");
        }
        assert_eq!(engine.lookup_waits(), 0, "{h}");
    }
}

/// Steps 1–7 over every reply (D23): every diff authenticates; with Core's `qrinfo`s every
/// quorum Core lists at the tip verifies, and so does every ChainLock Core verified. About 40 s
/// unoptimised, so it runs in the gate's feature step, not tier 1.
#[cfg(feature = "testnet-replay")]
#[test]
fn full_replay_of_steps_1_to_7_over_cores_replies() {
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let genesis = &f["diffs"]["from_genesis"][0];
    let start = u32_of(&genesis["height"]);
    engine.apply_diff(&decode(&genesis["payload"]), &chain, &cfg).expect("Core's diff");
    apply_steps(&mut engine, f, &chain, &cfg, start);
    for h in chain.tip() - cfg.h_keep()..=chain.tip() {
        assert!(engine.has_authenticated_set(BlockId(h)), "{h}");
    }
    // Retention asks for no list it held (D29): only work blocks below the diff from genesis.
    for r in engine.requests() {
        if let MnRequest::MnListDiff {
            target_height,
            ..
        } = r
        {
            assert!(target_height < start, "{r:?}");
        }
    }
    apply_qrinfos(&mut engine, f, &chain, &cfg);
    let states: Vec<_> = quorum_states(&engine, f)
        .into_iter()
        .filter(|(.., s)| !matches!(s, Some(QuorumState::Verified(_))))
        .collect();
    assert!(states.is_empty(), "{states:#?}");
    for c in f["chainlocks"].as_array().expect("chainlocks") {
        assert_eq!(c["core_verify"], true);
        let cl = clsig(c);
        let v = engine
            .verify_chainlock(&cl, &chain, &cfg)
            .unwrap_or_else(|e| panic!("{}: {e:?}", c["height"]));
        assert_eq!(v.block_hash, cl.block_hash);
    }
}

/// DEC-387 (Q5): the next rotation cycle's members read the newest cycle's kept work blocks
/// k = 0…2 with their own. With Core's rows for the captured cycles held (its `qrinfo`s' target
/// rows), the step replay verifies cycle 1574208's 32 rotated commitments as their diffs arrive,
/// reading its own work block 1574200 and cycle 1573920's 1573912, 1573624 and 1573336, kept
/// since that cycle: nothing is asked for those cycles, and no list at or above the diff from
/// genesis is fetched again. At the tip cycle 1574496 (none of its commitments mined yet) holds
/// its four work blocks whole, cycle 1574208's k = 0…2 among them.
#[cfg(feature = "testnet-replay")]
#[test]
fn the_next_rotation_cycle_reads_the_kept_lists() {
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let c = cfg.params().consensus.llmq(rotated).expect("configured").dkg_interval.unsigned_abs();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let mut rows = Vec::new();
    for q in f["qrinfos"].as_array().expect("qrinfos") {
        let reply: QrInfo = decode(&q["payload"]);
        let cycle = u32_of(&q["request_height"]);
        for (k, row) in
            [(1, &reply.snapshot_h_c), (2, &reply.snapshot_h_2c), (3, &reply.snapshot_h_3c)]
        {
            rows.push((cycle - k * c, crate::convert::snapshot(row)));
        }
    }
    // Held before each step, as a host answering each `qrinfo` at once would: retention keeps a
    // row only once a held commitment reads it (D45).
    let hold = |engine: &mut MasternodeListEngine| {
        for (height, row) in &rows {
            engine.hold_row(rotated, chain.hash(*height), row.clone());
        }
    };
    let genesis = &f["diffs"]["from_genesis"][0];
    let start = u32_of(&genesis["height"]);
    hold(&mut engine);
    engine.apply_diff(&decode(&genesis["payload"]), &chain, &cfg).expect("Core's diff");
    for h in start + 1..=chain.tip() {
        hold(&mut engine);
        apply_steps_to(&mut engine, f, &chain, &cfg, h - 1, h);
    }
    let (newest, next) = (1_574_208, 1_574_496);
    let mut verified: std::collections::BTreeMap<u32, usize> = Default::default();
    for (t, height, state) in quorum_states(&engine, f) {
        if t == rotated && height >= newest {
            assert!(matches!(state, Some(QuorumState::Verified(_))), "{height}: {state:?}");
            *verified.entry(height - height % c).or_insert(0) += 1;
        }
    }
    assert_eq!(verified, [(newest, 32)].into());
    for r in engine.requests() {
        match r {
            MnRequest::QrInfo {
                request_height,
                ..
            } => assert!(request_height < newest, "{r:?}"),
            MnRequest::MnListDiff {
                target_height,
                ..
            } => assert!(target_height < start, "{r:?}"),
        }
    }
    let whole = engine.whole_lists();
    for k in 0..4 {
        assert!(whole.contains(&(next - k * c - WORK_DIFF_DEPTH)), "{k}");
    }
}

/// The heap retention holds after the step replay (D28, D29; DEC-387 asks for it), by glibc's
/// in-use bytes: the engine's total, what each set-only height and each whole list adds, and the
/// rest (rows, requests). Run alone, as other tests' allocations would count:
/// `cargo test -p dash-mnlist --features testnet-replay -- --ignored --exact
/// tests::testnet::retained_memory_over_the_step_replay --nocapture`.
#[cfg(all(feature = "testnet-replay", target_os = "linux", target_env = "gnu"))]
#[test]
#[ignore = "measures the process heap: run alone"]
fn retained_memory_over_the_step_replay() {
    #[repr(C)]
    struct MallInfo2 {
        arena: usize,
        ordblks: usize,
        smblks: usize,
        hblks: usize,
        hblkhd: usize,
        usmblks: usize,
        fsmblks: usize,
        uordblks: usize,
        fordblks: usize,
        keepcost: usize,
    }
    unsafe extern "C" {
        fn mallinfo2() -> MallInfo2;
    }
    // SAFETY: glibc's `mallinfo2` takes nothing and returns its counters by value.
    let in_use = || {
        let m = unsafe { mallinfo2() };
        m.uordblks + m.hblkhd
    };
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let genesis = &f["diffs"]["from_genesis"][0];
    let start = u32_of(&genesis["height"]);
    let before = in_use();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&decode(&genesis["payload"]), &chain, &cfg).expect("Core's diff");
    apply_steps(&mut engine, f, &chain, &cfg, start);
    let total = in_use() - before;
    let whole = engine.whole_lists();
    let sets: Vec<u32> = engine.held_heights().into_iter().filter(|h| !whole.contains(h)).collect();
    let distinct = engine.distinct_sets();
    let rows = engine.row_count();
    for h in &sets {
        engine.forget_list(BlockId(*h));
    }
    let without_sets = in_use() - before;
    for h in &whole {
        engine.forget_list(BlockId(*h));
    }
    let without_lists = in_use() - before;
    drop(engine);
    let residue = in_use() - before;
    eprintln!(
        "retained: total {total} B; {} set-only heights, {distinct} distinct sets: {} B, {} B per \
         held height; {} whole lists: {} B, {} B each; {rows} rows and the rest {} B; after drop \
         {residue} B",
        sets.len(),
        total - without_sets,
        (total - without_sets) / sets.len(),
        whole.len(),
        without_sets - without_lists,
        (without_sets - without_lists) / whole.len(),
        without_lists - residue,
    );
}

/// BL-382: the persistence mirror's cost over the full replay (every reply, `qrinfo`s too), in
/// glibc in-use bytes: the engine, then the mirror the first `take_dirty` builds (its writes
/// dropped first), with the rotation rows and the bookkeeping counted. Run alone:
/// `cargo test -p dash-mnlist --features testnet-replay -- --ignored --exact
/// tests::testnet::stored_mirror_memory_over_the_full_replay --nocapture`.
#[cfg(all(feature = "testnet-replay", target_os = "linux", target_env = "gnu"))]
#[test]
#[ignore = "measures the process heap: run alone"]
fn stored_mirror_memory_over_the_full_replay() {
    #[repr(C)]
    struct MallInfo2 {
        arena: usize,
        ordblks: usize,
        smblks: usize,
        hblks: usize,
        hblkhd: usize,
        usmblks: usize,
        fsmblks: usize,
        uordblks: usize,
        fordblks: usize,
        keepcost: usize,
    }
    unsafe extern "C" {
        fn mallinfo2() -> MallInfo2;
    }
    // SAFETY: glibc's `mallinfo2` takes nothing and returns its counters by value.
    let in_use = || {
        let m = unsafe { mallinfo2() };
        m.uordblks + m.hblkhd
    };
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let genesis = &f["diffs"]["from_genesis"][0];
    let start = u32_of(&genesis["height"]);
    let before = in_use();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.apply_diff(&decode(&genesis["payload"]), &chain, &cfg).expect("Core's diff");
    apply_steps(&mut engine, f, &chain, &cfg, start);
    apply_qrinfos(&mut engine, f, &chain, &cfg);
    engine.tip_changed(&chain, &cfg).expect("step 5");
    let engine_bytes = in_use() - before;
    let writes = engine.take_dirty();
    let written: usize = writes.ops().map(|(k, v)| k.len() + v.map_or(0, <[u8]>::len)).sum();
    let ops = writes.ops().count();
    drop(writes);
    let mirror = in_use() - before - engine_bytes;
    let (rows, books) = engine.rotation_rows_and_bookkeeping();
    eprintln!(
        "engine {engine_bytes} B; mirror {mirror} B; first writes {ops} ops, {written} B; rotation \
         rows: {} cycles, {} snapshots, {} member bits; bookkeeping answered {} failed {} \
         dropped {} retries {}",
        rows[0], rows[1], rows[2], books[0], books[1], books[2], books[3],
    );
}

/// BL-380 (D42): the store writes per block over the step replay, each step's `take_dirty`
/// committed: bytes by table (lists, quorums, rows and marks) and whole lists put. Run with
/// `cargo test -p dash-mnlist --features testnet-replay --release -- --ignored --exact
/// tests::testnet::store_writes_per_block_over_the_step_replay --nocapture`.
#[cfg(feature = "testnet-replay")]
#[test]
#[ignore = "a measurement: run alone"]
fn store_writes_per_block_over_the_step_replay() {
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let genesis = &f["diffs"]["from_genesis"][0];
    let start = u32_of(&genesis["height"]);
    engine.apply_diff(&decode(&genesis["payload"]), &chain, &cfg).expect("Core's diff");
    let first: usize =
        engine.take_dirty().ops().map(|(k, v)| k.len() + v.map_or(0, <[u8]>::len)).sum();
    // Lists, sets, bases, records, rows, marks (D48).
    let (mut blocks, mut bytes, mut max) = (0usize, [0usize; 6], 0usize);
    let (mut sets_put, mut bases_put) = (0usize, 0usize);
    for d in f["diffs"]["step"].as_array().expect("steps") {
        let h = u32_of(&d["height"]);
        if h <= start {
            continue;
        }
        engine
            .apply_diff(&decode(&d["payload"]), &chain, &cfg)
            .unwrap_or_else(|e| panic!("{h}: {e}"));
        let w = engine.take_dirty();
        let mut block = 0;
        for (k, v) in w.ops() {
            let n = k.len() + v.map_or(0, <[u8]>::len);
            block += n;
            let i = match k[0] {
                b'L' => 0,
                b'S' => 1,
                b'B' => 2,
                b'Q' => 3,
                b'R' => 4,
                _ => 5,
            };
            bytes[i] += n;
            match k[0] {
                b'S' => sets_put += usize::from(v.is_some()),
                b'B' => bases_put += usize::from(v.is_some()),
                _ => {}
            }
        }
        max = max.max(block);
        blocks += 1;
    }
    let total: usize = bytes.iter().sum();
    let [lists, sets, bases, quorums, rows, marks] = bytes.map(|b| b / blocks);
    println!(
        "first {first} B; {blocks} blocks: {total} B, {} B a block (max {max}); a block: lists {lists} sets {sets} bases {bases} quorums {quorums} rows {rows} marks {marks}; sets put {sets_put}, bases put {bases_put}",
        total / blocks,
    );
}

/// D48 (validator s5): the step replay committed block by block to a store, which loads to the
/// live engine every 100 blocks and at the end: real removals and changes on a list of thousands
/// of entries, and bases past `BASE_SPAN`, through the delta codec.
#[cfg(feature = "testnet-replay")]
#[test]
fn full_replay_store_round_trips_block_by_block() {
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let store = super::persist::Store::new();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let genesis = &f["diffs"]["from_genesis"][0];
    let start = u32_of(&genesis["height"]);
    engine.apply_diff(&decode(&genesis["payload"]), &chain, &cfg).expect("Core's diff");
    store.commit(&engine.take_dirty());
    let (mut blocks, mut bases) = (0usize, 0usize);
    for d in f["diffs"]["step"].as_array().expect("steps") {
        let h = u32_of(&d["height"]);
        if h <= start {
            continue;
        }
        engine
            .apply_diff(&decode(&d["payload"]), &chain, &cfg)
            .unwrap_or_else(|e| panic!("{h}: {e}"));
        let w = engine.take_dirty();
        bases += w.ops().filter(|(k, v)| k[0] == b'B' && v.is_some()).count();
        store.commit(&w);
        blocks += 1;
        if blocks % 100 == 0 {
            store.assert_holds(&chain, &mut engine);
        }
    }
    store.assert_holds(&chain, &mut engine);
    assert!(bases >= 2, "a base past the span: {bases}");
}

/// Steps 1–7 over the last rotation cycle (D23): Core's `qrinfo`s, then its step diffs from the
/// newest one's H work block to the tip. Every target of Core's replies is authenticated. The
/// rotated quorums verify from the `qrinfo`s; a non-rotated one verifies exactly when a reply
/// targets its work block (base − 8 from V20, Core's `GetAllQuorumMembers`), the newest via the
/// step diffs, and waits on that list otherwise. The ChainLocks selecting a verified quorum
/// verify; every other one waits.
#[test]
fn steps_1_to_7_over_the_last_rotation_cycle() {
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    apply_qrinfos(&mut engine, f, &chain, &cfg);
    let work = newest_work_block(f, &chain);
    apply_steps(&mut engine, f, &chain, &cfg, work);
    // The lists Core's replies target, from the decoded payloads alone.
    let mut targets = BTreeSet::new();
    for q in f["qrinfos"].as_array().expect("qrinfos") {
        let reply: QrInfo = decode(&q["payload"]);
        let diffs =
            [&reply.diff_tip, &reply.diff_h, &reply.diff_h_c, &reply.diff_h_2c, &reply.diff_h_3c]
                .into_iter()
                .chain(reply.cycle_h_4c.as_ref().map(|c| &c.diff))
                .chain(&reply.mn_list_diff_list);
        for d in diffs {
            targets.insert(chain.lookup(&d.block_hash).expect("ok").expect("a captured header").0);
        }
    }
    for d in f["diffs"]["step"].as_array().expect("steps") {
        let height = u32_of(&d["height"]);
        if height > work {
            targets.insert(height);
        }
    }
    assert!((work..=chain.tip()).all(|h| targets.contains(&h)));
    for h in &targets {
        assert!(engine.has_authenticated_set(BlockId(*h)), "{h}");
    }

    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let v20 = cfg.params().consensus.deployment_height(BuriedDeployment::V20).to_u32();
    let mut held_by_steps = false;
    for (llmq, height, state) in quorum_states(&engine, f) {
        let verified = matches!(state, Some(QuorumState::Verified(_)));
        if llmq == rotated {
            assert!(verified, "{llmq:?} {height}: {state:?}");
            continue;
        }
        assert!(height + 1 >= v20, "V20 active after the base");
        let at = height - WORK_DIFF_DEPTH;
        let held = targets.contains(&at);
        assert_eq!(verified, held, "{llmq:?} {height}: {state:?}");
        assert!(
            held || matches!(state, Some(QuorumState::Pending(_))),
            "{llmq:?} {height}: {state:?}"
        );
        held_by_steps |= held && at > work;
    }
    assert!(held_by_steps, "no quorum verified over the step diffs");

    let mut locked = Vec::new();
    for c in f["chainlocks"].as_array().expect("chainlocks") {
        let cl = clsig(c);
        match engine.verify_chainlock(&cl, &chain, &cfg) {
            Ok(v) => {
                assert_eq!(v.block_hash, cl.block_hash);
                locked.push(u32_of(&c["height"]));
            }
            Err(e) => assert_eq!(
                e,
                LockOutcome::Rejected(LockReject::QuorumUnavailable),
                "{}",
                c["height"]
            ),
        }
    }
    locked.sort_unstable();
    assert_eq!(locked, [1574501, 1574503, 1574516]);
}

/// A captured ChainLock moved to `height` (its signature no longer Core's).
fn moved(f: Capture, chain: &TestnetChain, height: u32) -> ClSig {
    let mut cl = clsig(&f["chainlocks"][0]);
    cl.height = i32::try_from(height).expect("a height");
    cl.block_hash = chain.hash(height);
    cl
}

/// Core's ChainLock at `height`.
#[cfg(feature = "testnet-replay")]
fn captured_chainlock(f: Capture, height: u32) -> ClSig {
    let c = f["chainlocks"]
        .as_array()
        .expect("chainlocks")
        .iter()
        .find(|c| u32_of(&c["height"]) == height);
    clsig(c.expect("captured"))
}

/// The commitments of type `t` Core's step diff to `height` mines and deletes, by base height.
#[cfg(feature = "testnet-replay")]
fn mined_and_deleted(
    f: Capture,
    chain: &TestnetChain,
    t: LlmqType,
    height: u32,
) -> (Vec<u32>, Vec<u32>) {
    let diff = step_diff_of(f, height);
    let base = |hash: &BlockHash| chain.lookup(hash).expect("ok").expect("a base").0;
    let mined = diff.new_quorums.iter().filter(|q| q.llmq_type == t);
    let deleted = diff.deleted_quorums.iter().filter(|q| q.llmq_type == t);
    (
        mined.map(|q| base(&BlockHash::from(*q.quorum_hash.as_bytes()))).collect(),
        deleted.map(|q| base(&q.hash)).collect(),
    )
}

/// The engine `checks` verifies a ChainLock with: Core's `qrinfo`s, then Core's step diffs from
/// the newest list they hold at or below `from` to `chain`'s tip, with each block in `pins` kept
/// whole, a later fetch's base (D29).
#[cfg(feature = "testnet-replay")]
fn stepped_from(
    f: Capture,
    chain: &TestnetChain,
    cfg: &LlmqConfig,
    from: u32,
    pins: &[u32],
) -> MasternodeListEngine {
    let mut engine = MasternodeListEngine::new(Mode::Client);
    for q in f["qrinfos"].as_array().expect("qrinfos") {
        if u32_of(&q["request_height"]) <= chain.tip() {
            let reply: QrInfo = decode(&q["payload"]);
            engine.apply_qrinfo(&reply, chain, cfg).expect("Core's qrinfo");
        }
    }
    let base =
        (chain.floor..=from).rev().find(|h| engine.list(BlockId(*h)).is_some()).expect("a list");
    apply_steps_held(&mut engine, f, chain, cfg, (base, chain.tip()), pins);
    engine
}

/// The set at `s` dropped, `cl` asks for it once from the list below, and verifies once Core's
/// step diff to it is applied; `others` verify throughout with no request.
#[cfg(feature = "testnet-replay")]
fn refetched(
    engine: &mut MasternodeListEngine,
    f: Capture,
    chain: &TestnetChain,
    cfg: &LlmqConfig,
    cl: &ClSig,
    s: u32,
    others: &[&ClSig],
) {
    engine.forget_list(BlockId(s));
    let mut expected = engine.requests();
    for _ in 0..3 {
        assert_eq!(
            engine.verify_chainlock(cl, chain, cfg),
            Err(LockOutcome::Rejected(LockReject::QuorumUnavailable)),
            "{s}"
        );
    }
    expected.push(MnRequest::MnListDiff {
        base: Some(chain.hash(s - 1)),
        target: chain.hash(s),
        target_height: s,
    });
    let mut asked = engine.requests();
    asked.sort_by_key(|r| r.key());
    expected.sort_by_key(|r| r.key());
    assert_eq!(asked, expected, "{s}: one fetch, of the set");
    for other in others {
        assert!(engine.verify_chainlock(other, chain, cfg).is_ok(), "{s}: {}", other.height);
    }
    apply_steps_to(engine, f, chain, cfg, s - 1, s);
    let v = engine.verify_chainlock(cl, chain, cfg).expect("Core's verdict");
    assert_eq!((v.height, v.block_hash), (cl.height, cl.block_hash));
}

/// PLAN SB11 row item 8a, Sol r3's case over Core's bytes (SB0 §5.2.1, DEC-107 M4, D52,
/// slice 8). The type 1 commitment Core mines at 1,574,387 deletes the quorum based at 1,573,800,
/// the oldest of the 24 active; Core's ChainLock for 1,574,394 selects at 1,574,386, before the
/// mine, and that quorum signs it. Delivered at tip 1,574,395, whose own selection start is the
/// mine, it verifies against the replaced quorum from the retained set, asking for nothing; with
/// that set dropped it verifies after one fetch. The quorum's work block is reached only from the
/// diff from genesis (about 1,160 diffs unoptimised), so it runs in the gate's feature step.
#[cfg(feature = "testnet-replay")]
#[test]
fn sol_r3s_case_verifies_from_the_retained_set_and_after_one_fetch() {
    const MINE: u32 = 1_574_387;
    let f = testnet();
    let cfg = config();
    let lock = MINE + SIGN_HEIGHT_OFFSET - 1;
    let full = TestnetChain::new(f);
    let chain = full.window(full.floor, lock + 1);
    let cl_type = cfg.params().consensus.llmq_type_chain_locks;
    let (mined, deleted) = mined_and_deleted(f, &chain, cl_type, MINE);
    assert_eq!((mined, deleted), (vec![1_574_376], vec![1_573_800]));
    let s = lock - SIGN_HEIGHT_OFFSET;
    assert_eq!(s, MINE - 1);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let genesis = &f["diffs"]["from_genesis"][0];
    engine.apply_diff(&decode(&genesis["payload"]), &chain, &cfg).expect("Core's diff");
    let start = u32_of(&genesis["height"]);
    apply_steps_held(&mut engine, f, &chain, &cfg, (start, chain.tip()), &[s - 1]);
    let cl = captured_chainlock(f, lock);
    let requests = engine.requests();
    let v = engine.verify_chainlock(&cl, &chain, &cfg).expect("Core's verdict");
    assert_eq!(
        v.quorum.quorum_hash(),
        &QuorumHash::from(*chain.hash(1_573_800).as_bytes()),
        "the replaced quorum"
    );
    assert_eq!(engine.requests(), requests, "from the retained set");
    refetched(&mut engine, f, &chain, &cfg, &cl, s, &[]);
}

/// PLAN SB11 row item 8b (SB0 §5.2.1, D52, slice 8): delayed ChainLocks immediately before and
/// after a commitment mine. Core mines a type 1 commitment at 1,574,507; its ChainLocks for
/// 1,574,514 and 1,574,515, delivered at the capture's tip 18 blocks later, select at 1,574,506
/// (before the mine) and 1,574,507 (its block). Each verifies from its own set: with one side's set
/// dropped, its lock asks for that set alone and verifies after the fetch, while the other side's
/// verifies with no request. About 330 diffs and their step 5s unoptimised, so it runs in the
/// gate's feature step.
#[cfg(feature = "testnet-replay")]
#[test]
fn delayed_chainlocks_either_side_of_a_mine_read_their_own_sets() {
    const MINE: u32 = 1_574_507;
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let cl_type = cfg.params().consensus.llmq_type_chain_locks;
    let (mined, _) = mined_and_deleted(f, &chain, cl_type, MINE);
    assert_eq!(mined, [1_574_496]);
    let before = captured_chainlock(f, MINE - 1 + SIGN_HEIGHT_OFFSET);
    let after = captured_chainlock(f, MINE + SIGN_HEIGHT_OFFSET);
    // From the `qrinfo`s' list at 1,574,200: the signers' work blocks are above it.
    let mut engine = stepped_from(f, &chain, &cfg, 1_574_200, &[MINE - 2, MINE - 1]);
    for cl in [&before, &after] {
        let v = engine.verify_chainlock(cl, &chain, &cfg);
        assert!(v.is_ok(), "{}: {v:?} {:?}", cl.height, engine.requests());
    }
    refetched(&mut engine, f, &chain, &cfg, &after, MINE, &[&before]);
    refetched(&mut engine, f, &chain, &cfg, &before, MINE - 1, &[&after]);
}

/// Coverage and the recent-lock horizon over the last rotation cycle (D24, D25). The domain's
/// oldest end, tip − H_keep, is asked for from the `qrinfo` work block below it, and once held
/// the domain is covered; its deepest required block is the k = 3 work block of the rotated cycle
/// active there (1573920). A delayed ChainLock checked against a set it stored before the type 1
/// commitment mined at 1574507 asks for that set once, however many locks arrive, waits while the
/// fetch fails, and verifies once it is applied. A start one block below the horizon asks for
/// nothing; one at the horizon reads its held set (where the quorum it selects is unverified).
#[test]
fn coverage_and_the_horizon_over_the_last_rotation_cycle() {
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let tip = chain.tip();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    // The base of the fetch below, which Core's step diff answers (D29: a request keeps its base).
    engine.pin_list(BlockId(STORE - 1));
    apply_qrinfos(&mut engine, f, &chain, &cfg);
    apply_steps(&mut engine, f, &chain, &cfg, newest_work_block(f, &chain));

    let oldest = tip - cfg.h_keep();
    assert_eq!(engine.coverage(&chain, &cfg), Ok(Coverage::Pending));
    let base = (chain.floor..oldest)
        .rev()
        .find(|h| engine.has_authenticated_set(BlockId(*h)))
        .expect("a qrinfo's work block below");
    let ask = MnRequest::MnListDiff {
        base: Some(chain.hash(base)),
        target: chain.hash(oldest),
        target_height: oldest,
    };
    assert!(engine.requests().contains(&ask), "{:?}", engine.requests());
    apply_steps_to(&mut engine, f, &chain, &cfg, base, oldest);
    assert_eq!(engine.coverage(&chain, &cfg), Ok(Coverage::Covered));
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let c = cfg.params().consensus.llmq(rotated).expect("configured").dkg_interval.unsigned_abs();
    let deepest = 1_573_920 - 3 * c - WORK_DIFF_DEPTH;
    assert_eq!(engine.coverage(&chain.window(deepest, tip), &cfg), Ok(Coverage::Covered));
    assert_eq!(
        engine.coverage(&chain.window(deepest + 1, tip), &cfg),
        Ok(Coverage::Below {
            lowest: Some(deepest)
        })
    );

    // 1574501 selects at 1574493, stored at the type 1 mining window's end, 1574490; every set
    // from there to the start gives the type's entries (D13), so all four are forgotten.
    let c = f["chainlocks"]
        .as_array()
        .expect("chainlocks")
        .iter()
        .find(|c| u32_of(&c["height"]) == 1_574_501);
    let cl = clsig(c.expect("captured"));
    let store = STORE;
    for at in store..=1_574_493 {
        engine.forget_list(BlockId(at));
    }
    let before = engine.requests().len();
    for _ in 0..1000 {
        assert_eq!(
            engine.verify_chainlock(&cl, &chain, &cfg),
            Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
        );
    }
    let fetch = MnRequest::MnListDiff {
        base: Some(chain.hash(store - 1)),
        target: chain.hash(store),
        target_height: store,
    };
    assert_eq!(engine.requests().len(), before + 1);
    assert!(engine.requests().contains(&fetch));
    engine.request_failed(fetch.key());
    assert_eq!(
        engine.verify_chainlock(&cl, &chain, &cfg),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    assert!(!engine.requests().contains(&fetch), "a failed fetch is not asked again");
    apply_steps_to(&mut engine, f, &chain, &cfg, store - 1, store);
    assert!(engine.verify_chainlock(&cl, &chain, &cfg).is_ok());

    let requests = engine.requests();
    let outside = moved(f, &chain, oldest - 1 + SIGN_HEIGHT_OFFSET);
    assert_eq!(
        engine.verify_chainlock(&outside, &chain, &cfg),
        Err(LockOutcome::Rejected(LockReject::OutsideHorizon))
    );
    assert_eq!(engine.set_at(BlockId(oldest - 1), &chain, &cfg), Ok(SetAt::OutsideHorizon));
    assert_eq!(engine.requests(), requests);
    let inside = moved(f, &chain, oldest + SIGN_HEIGHT_OFFSET);
    assert_eq!(
        engine.verify_chainlock(&inside, &chain, &cfg),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    assert_eq!(engine.set_at(BlockId(oldest), &chain, &cfg), Ok(SetAt::Held));
}

/// A later coverage miss (D26). Anchored just above a type 1 cycle base, Core's diff from genesis
/// holds commitments whose base or work block lies below the anchor: each is `Unavailable`, and
/// the payload raises exactly one diagnostic for each, naming the missing height when the base is
/// known. The next payload raises none again. A set more than H_keep below the tip raises none
/// until the tip comes back within H_keep of it.
#[test]
fn a_later_miss_raises_one_diagnostic_per_commitment() {
    let f = testnet();
    let full = TestnetChain::new(f);
    let cfg = config();
    let genesis = &f["diffs"]["from_genesis"][0];
    let start = u32_of(&genesis["height"]);
    let floor = FLOOR_ABOVE_A_BASE;
    let chain = full.window(floor, start + 1);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let change =
        engine.apply_diff(&decode(&genesis["payload"]), &chain, &cfg).expect("Core's diff");
    let mut named = false;
    let mut raised = HashSet::new();
    for e in &change.events {
        let MnEvent::CoverageMiss {
            llmq_type,
            quorum_hash,
            anchor,
            missing,
        } = e;
        assert_eq!(*anchor, floor);
        let state = engine.quorum(*llmq_type, quorum_hash).expect("held").1;
        let expected = match missing {
            Some(height) => QuorumState::Unavailable(Unavailable::BelowFloor {
                height: *height,
            }),
            None => QuorumState::Unavailable(Unavailable::UnknownBase),
        };
        assert_eq!(state, expected, "{llmq_type:?}");
        named |= missing.is_some_and(|h| h < floor && h + WORK_DIFF_DEPTH >= floor);
        assert!(raised.insert((*llmq_type, *quorum_hash)), "raised twice: {e:?}");
    }
    assert!(named, "a known base's work block below the anchor is named");
    let missed: HashSet<_> = change
        .new_quorums
        .iter()
        .filter(|(t, q)| {
            matches!(
                engine.quorum(*t, q).expect("held").1,
                QuorumState::Unavailable(Unavailable::BelowFloor { .. } | Unavailable::UnknownBase)
            )
        })
        .copied()
        .collect();
    assert!(!missed.is_empty());
    assert_eq!(raised, missed, "one diagnostic for each missed commitment");
    let next = engine
        .apply_diff(&decode(&f["diffs"]["step"][0]["payload"]), &chain, &cfg)
        .expect("the next diff");
    assert!(next.events.is_empty(), "{:?}", next.events);

    let events = |tip: u32| {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        let at = full.window(floor, tip);
        let change = engine.apply_diff(&decode(&genesis["payload"]), &at, &cfg).expect("Core's");
        change.events.len()
    };
    assert_eq!(events(start + cfg.h_keep()), raised.len(), "the set at tip − H_keep is within");
    assert_eq!(events(start + cfg.h_keep() + 1), 0, "below tip − H_keep");

    let far = full.window(floor, full.tip());
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let change = engine.apply_diff(&decode(&genesis["payload"]), &far, &cfg).expect("Core's diff");
    assert!(change.events.is_empty(), "outside the horizon");
    let back = engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(back.events.len(), raised.len(), "raised once the set is within the horizon");
    assert!(engine.tip_changed(&chain, &cfg).expect("step 5").events.is_empty());
}

/// `MnChain::anchor` for SB4's `HeaderChain` (D25): anchored on the capture's lowest 25 headers,
/// its floor is the lowest and its anchor the 25th, above the pack's 24 context entries.
#[test]
fn an_anchored_header_chains_anchor_is_above_its_context_entries() {
    let f = testnet();
    let floor = u32_of(&f["floor"]);
    let raw: Vec<BlockHeader> = f["headers"].as_array().expect("headers")[..25]
        .iter()
        .map(|h| {
            let bytes = hex(h.as_str().expect("a header"));
            *decode_header(&mut &bytes[..]).expect("a header").into_value().raw()
        })
        .collect();
    let cfg = config();
    let params = cfg.params_arc().clone();
    let pack = AnchorPack {
        height: floor + 24,
        hash: Header::new(raw[24]).hash(),
        chain_work: Work::from_u64(u64::MAX),
        headers: raw[..].try_into().expect("25 headers"),
    };
    let svc = super::step7::services(&params, &raw);
    let spv = ChainConfig::spv(&params, 10_000);
    let chain = HeaderChain::<NoStore, Evicting>::new_anchored(params, spv, svc, NoStore, &pack)
        .expect("an anchor");
    assert_eq!(chain.anchored().floor(), floor);
    assert_eq!(MnChain::anchor(&chain), floor + 24);
}

/// BL-390, D51 over Core's bytes. The type 6 quorum based at 1574496, mined at 1574507, is run
/// through step 5 again with the list at its work block 1574488 dropped: step 5 asks for it and the
/// host gives up on it (`FetchFailed`). Platform's call for the lookup at 1574507 asks for the list
/// again, once per wait, with the lookup's targets not held (at 1574508 neither, at 1574507 the one
/// at 1574469; validator s7 M1); the wait stays while it is out, a failure the wait owns ends it
/// `Unavailable`, not `Fetch`, and the next call asks once more. With both targets held (1574469's
/// set authenticated as held, without the quorum Core mined later; the capture has no diff to it)
/// the failure reads `Unavailable` at once. Core's diff from genesis to 1574488 (the newest
/// `qrinfo`'s) answers it, and the lookup finds the quorum with Core's `quorum info` key.
#[test]
fn a_platform_lookup_asks_again_for_its_holders_failed_input() {
    const MINED: u32 = 1_574_507;
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let platform = cfg.params().consensus.llmq_type_platform;
    let mut engine = MasternodeListEngine::new(Mode::Client);
    apply_qrinfos(&mut engine, f, &chain, &cfg);
    let work = newest_work_block(f, &chain);
    apply_steps_to(&mut engine, f, &chain, &cfg, work, MINED - 1);
    let core = f["quorums"][platform.to_base().to_string()]
        .as_array()
        .expect("Core's quorums")
        .iter()
        .find(|q| u32_of(&q["height"]) == 1_574_496)
        .expect("Core lists it");
    let hash = QuorumHash::from(rpc(core["quorumHash"].as_str().expect("a hash")));
    assert_eq!(work, 1_574_496 - WORK_DIFF_DEPTH, "its work block");
    apply_steps_to(&mut engine, f, &chain, &cfg, MINED - 1, MINED);
    let state = |e: &MasternodeListEngine| e.quorum(platform, &hash).expect("held").1;
    let (qc, verified) = engine.quorum(platform, &hash).expect("held");
    assert!(matches!(verified, QuorumState::Verified(_)), "the qrinfos' sets hold it");
    // As retention would drop the list, and a reload reset the verdict (D38): step 5 asks.
    engine.forget_list(BlockId(work));
    engine.reset_state(&qc);
    engine.tip_changed(&chain, &cfg).expect("step 5");
    let key = RequestKey::MnListDiff(chain.hash(work));
    assert_eq!(state(&engine), QuorumState::Pending(key));
    let low = MINED - cfg.w_p();
    let lookup = |e: &MasternodeListEngine| e.lookup_quorum(platform, &hash, MINED, &chain, &cfg);
    let out = |e: &MasternodeListEngine, k| e.requests().iter().filter(|r| r.key() == k).count();
    let asks = |e: &MasternodeListEngine| out(e, key);
    let fetch = |e: &mut MasternodeListEngine| {
        e.fetch_lookup(platform, &hash, MINED, &chain, &cfg).expect("not busy")
    };
    // Neither target of the lookup at MINED + 1 held: its wait stays open while the input asked
    // again is out, though both targets failed, and that failure, its own, ends it `Unavailable`.
    let next = MINED + 1;
    let at_next = |e: &MasternodeListEngine| {
        e.lookup_quorum(platform, &hash, next, &chain, &cfg).expect("held")
    };
    let targets = [next - cfg.w_p(), next].map(|h| RequestKey::MnListDiff(chain.hash(h)));
    engine.request_failed(key);
    engine.fetch_lookup(platform, &hash, next, &chain, &cfg).expect("not busy");
    assert_eq!(state(&engine), QuorumState::Pending(key));
    assert!(targets.iter().all(|k| out(&engine, *k) == 1), "both targets asked");
    for k in targets {
        engine.request_failed(k);
    }
    assert_eq!(engine.lookup_waits(), 1, "the input asked again is out");
    assert_eq!(at_next(&engine), Lookup::Fetch);
    engine.request_failed(key);
    assert_eq!(engine.lookup_waits(), 0, "its own failure ends it");
    assert_eq!(at_next(&engine), Lookup::Unavailable);
    // The target at `low` not held: asked with the input, and its failure ends the wait.
    let low_key = RequestKey::MnListDiff(chain.hash(low));
    engine.request_failed(key);
    assert_eq!(state(&engine), QuorumState::Unavailable(Unavailable::FetchFailed));
    assert_eq!(lookup(&engine).expect("held"), Lookup::Fetch, "a target to fetch");
    fetch(&mut engine);
    assert_eq!(state(&engine), QuorumState::Pending(key));
    assert_eq!((asks(&engine), out(&engine, low_key)), (1, 1), "both asked");
    engine.request_failed(key);
    assert_eq!(lookup(&engine).expect("held"), Lookup::Fetch, "the target is out");
    fetch(&mut engine);
    assert_eq!(asks(&engine), 0, "once per wait");
    engine.request_failed(low_key);
    // Nothing is out and the target at MINED is held, so the wait stays to the next call; it is
    // spent, so the lookup reads `Unavailable` and a call asks nothing (Opus s7 r1 MAJOR-1).
    assert_eq!(asks(&engine) + out(&engine, low_key), 0, "nothing out");
    assert_eq!(engine.lookup_waits(), 1);
    assert_eq!(lookup(&engine).expect("held"), Lookup::Unavailable, "spent, not Fetch");
    fetch(&mut engine);
    assert_eq!(asks(&engine) + out(&engine, low_key), 0, "nothing asked again");
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(engine.lookup_waits(), 0);
    assert_eq!(lookup(&engine).expect("held"), Lookup::Unavailable, "answered, not Fetch");
    // Both targets held: each failure is `Unavailable` at once; the wait goes at the next call,
    // and the next fetch asks once more.
    engine.hold_set(BlockId(low), low, chain.hash(low), &[]);
    for _ in 0..2 {
        fetch(&mut engine);
        assert_eq!(state(&engine), QuorumState::Pending(key));
        assert_eq!(asks(&engine), 1, "asked again");
        assert_eq!(lookup(&engine).expect("held"), Lookup::Fetch);
        engine.request_failed(key);
        assert_eq!(state(&engine), QuorumState::Unavailable(Unavailable::FetchFailed));
        assert_eq!(lookup(&engine).expect("held"), Lookup::Unavailable);
        fetch(&mut engine);
        assert_eq!(asks(&engine), 0, "once per wait");
        assert_eq!(engine.lookup_waits(), 1);
        engine.tip_changed(&chain, &cfg).expect("step 5");
        assert_eq!(engine.lookup_waits(), 0);
        assert_eq!(lookup(&engine).expect("held"), Lookup::Unavailable);
    }
    fetch(&mut engine);
    assert_eq!(asks(&engine), 1);
    let newest = f["qrinfos"]
        .as_array()
        .expect("qrinfos")
        .iter()
        .max_by_key(|q| u32_of(&q["request_height"]))
        .expect("a qrinfo");
    let reply: QrInfo = decode(&newest["payload"]);
    assert_eq!(reply.diff_h.block_hash, chain.hash(work));
    engine.apply_diff(&reply.diff_h, &chain, &cfg).expect("Core's diff");
    match lookup(&engine).expect("held") {
        Lookup::Found(v) => {
            assert_eq!(v.quorum_hash(), &hash, "never another quorum's key");
            let key = v.public_key().to_bytes(dash_pkc::bls::BlsSchemeKind::Basic);
            let cores = hex(core["quorumPublicKey"].as_str().expect("a key"));
            assert_eq!(key.as_slice(), cores.as_slice(), "Core's key");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(engine.lookup_waits(), 0);
}

/// D37, SB0 §5.3 end to end over Core's bytes. With the step diffs applied through 1574506 and
/// headers to the tip, Platform's lookup of the type 6 quorum based at 1574496, at the block
/// mining it (1574507), is `Fetch`. Its fetch asks the active chain's 1574469 (h − W_P) and
/// 1574507 from the newest whole list below it, 1574506: Core's step diff to 1574507. That reply
/// alone finds the quorum, with the key Core's `quorum info` gives.
#[test]
fn a_platform_lookup_fetches_cores_step_diff_and_finds_cores_key() {
    const MINED: u32 = 1_574_507;
    let f = testnet();
    let chain = TestnetChain::new(f);
    let cfg = config();
    let platform = cfg.params().consensus.llmq_type_platform;
    let mut engine = MasternodeListEngine::new(Mode::Client);
    apply_qrinfos(&mut engine, f, &chain, &cfg);
    apply_steps_to(&mut engine, f, &chain, &cfg, newest_work_block(f, &chain), MINED - 1);
    let core = f["quorums"][platform.to_base().to_string()]
        .as_array()
        .expect("Core's quorums")
        .iter()
        .find(|q| u32_of(&q["height"]) == 1_574_496)
        .expect("Core lists it");
    let hash = QuorumHash::from(rpc(core["quorumHash"].as_str().expect("a hash")));
    let lookup = |engine: &MasternodeListEngine| {
        engine.lookup_quorum(platform, &hash, MINED, &chain, &cfg).expect("held")
    };
    assert_eq!(lookup(&engine), Lookup::Fetch);
    let step = f["diffs"]["step"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|d| u32_of(&d["height"]) == MINED)
        .expect("Core's step diff");
    let reply: MnListDiff = decode(&step["payload"]);
    let low = MINED - cfg.w_p();
    let low_base = engine
        .whole_lists()
        .into_iter()
        .filter(|h| *h < low && engine.has_authenticated_set(BlockId(*h)))
        .max()
        .expect("a whole list below h - W_P");
    let before: BTreeSet<_> = engine.requests().iter().map(MnRequest::key).collect();
    engine.fetch_lookup(platform, &hash, MINED, &chain, &cfg).expect("not busy");
    let height = |r: &MnRequest| match r {
        MnRequest::MnListDiff {
            target_height,
            ..
        } => *target_height,
        MnRequest::QrInfo {
            ..
        } => panic!("a lookup asks diffs only"),
    };
    let mut asked: Vec<_> =
        engine.requests().into_iter().filter(|r| !before.contains(&r.key())).collect();
    asked.sort_by_key(height);
    let heights: Vec<u32> = asked.iter().map(height).collect();
    assert_eq!(heights, [low, MINED]);
    assert_eq!(
        asked[0],
        MnRequest::MnListDiff {
            base: Some(chain.hash(low_base)),
            target: chain.hash(low),
            target_height: low,
        },
        "from the newest whole list below it (Opus s6 r1 n2)"
    );
    assert_eq!(
        asked[1],
        MnRequest::MnListDiff {
            base: Some(reply.base_block_hash),
            target: reply.block_hash,
            target_height: MINED,
        },
        "Core's step diff answers it"
    );
    assert_eq!(u32_of(&step["base_height"]), MINED - 1);
    engine.apply_diff(&reply, &chain, &cfg).expect("Core's diff");
    match lookup(&engine) {
        Lookup::Found(v) => {
            assert_eq!(v.quorum_hash(), &hash, "never another quorum's key");
            let key = v.public_key().to_bytes(dash_pkc::bls::BlsSchemeKind::Basic);
            let cores = hex(core["quorumPublicKey"].as_str().expect("a key"));
            assert_eq!(key.as_slice(), cores.as_slice(), "Core's key");
        }
        other => panic!("{other:?}"),
    }
    assert!(engine.waiting_lookups().is_empty());
}

//! D-C3-8: Core's `ProcessNewBlockHeaders` verdicts from the `sb4-core-scenarios` harness
//! (SB4 §15.1, `contrib/node/sb4-core-scenarios`), replayed through `accept` on a node chain
//! started from genesis.
//!
//! Every `pnbh` (an op, or a `build` with `submit: "headers"`) is replayed and compared: the
//! result class, reason and debug message, `last_accepted`, and the view after it (tip, best
//! header, failed blocks, ChainLock floor, every `nStatus` that changed and the block-tree
//! records the batch changed: those it wrote whose bytes differ, as the harness compares the
//! block tree before and after). `build` with `submit: "connect"` accepts each header and connects
//! it. The other ops (`cl`, `spork19`, `timer`, `invalidate`, `reconsider`, `checkpoint`, …)
//! are not replayed: their recorded view is injected instead (tip, best header, statuses,
//! failed blocks, ChainLock best and floor, the live spork), and `checkpoint` patches the
//! checkpoint as Core's harness does. `reload` reloads the chain from its own records.
//!
//! A vector runs on its network with the arguments Core ran it with (its scenario file, checked
//! against the vector's provenance). Its `setup` step adds what Core's setup holds besides
//! genesis: a devnet's block 1, or a preloaded prefix of real headers (`tests/data/header-slices`).
//!
//! A-2's acceptance half replays `a2-pnbh-rules-anchored` on anchored SPV chains instead, and
//! A-38's on windowed SPV chains from genesis. The `headers`-mode `pnbh-*` vectors (one real
//! batch) replay on committed slices. `coverage.rs` holds the register of what is replayed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use dash_clock::MockClock;
use dash_consensus::BlockValidationResult as R;
use dash_consensus::pow::block_proof;
use dash_consensus::primitives::read_varint_u32;
use dash_params::{ChainOption, ChainParamsBuilder, Network, Release};
use dash_types::codec::BaseCodec as _;
use serde_json::Value;

use super::*;
use crate::config::StaticSporks;
use crate::mem_store::MemHeaderStore;
use crate::testing::{Latch, batch, fake_hash, params, services_at, services_with};

use super::spv_tests::{self, Spv};

/// Declares a replay of Core's vectors (`coverage.rs`): a plain `#[test]` (the pattern takes
/// doc comments and `#[replays(..)]` only, so no `#[ignore]` or `#[cfg]`) that must load exactly
/// the vectors `#[replays(..)]` yields, and a module of the same name holding its registration,
/// `REPLAYS`, which `coverage::REPLAYED` lists. The body runs as a function of its own, so a
/// `return` in it still reaches the end check.
macro_rules! scenario_test {
    ($(#[doc = $doc:literal])* #[replays($vectors:expr)] fn $name:ident() $body:block) => {
        $(#[doc = $doc])*
        #[test]
        fn $name() {
            fn scenario_test_body() $body
            $crate::chain::scenario_tests::coverage::Replays::begin(&$name::REPLAYS);
            scenario_test_body();
            $crate::chain::scenario_tests::coverage::Replays::end(&$name::REPLAYS);
        }

        #[doc = concat!("`", stringify!($name), "`'s registration.")]
        pub(in $crate::chain::scenario_tests) mod $name {
            #[allow(unused_imports, reason = "for the vectors' expression")]
            use super::*;

            pub(in $crate::chain::scenario_tests) const REPLAYS:
                $crate::chain::scenario_tests::coverage::Replays =
                $crate::chain::scenario_tests::coverage::Replays::new(module_path!(), || {
                    ($vectors).into_iter().collect()
                });
        }
    };
}

mod cl;
mod coverage;
mod rule_rows;

/// `pnbh` steps not compared, as (vector, step index, why). Each still runs, so the index
/// stays in step with Core's; the replay asserts this is exactly the set it skipped.
const SKIPPED: &[(&str, usize, &str)] = &[];

scenario_test! {
    #[replays(["a1a-pnbh-rules-genesis"])]
    fn replay_a1a_pnbh_rules_genesis() {
        assert_eq!(replay("a1a-pnbh-rules-genesis"), 12);
    }
}

scenario_test! {
    /// A-1a on a devnet (`-devnet=sb4`): block 1 must be the devnet genesis, and the
    /// `-minimumdifficultyblocks` window either side of its edge at the retarget height 1,152.
    #[replays(["a1a-devnet-min-difficulty-window", "a1a-devnet-min-difficulty-window-edge"])]
    fn replay_a1a_devnet() {
        assert_eq!(replay("a1a-devnet-min-difficulty-window"), 4);
        assert_eq!(replay("a1a-devnet-min-difficulty-window-edge"), 4);
    }
}

scenario_test! {
    /// A-1a on testnet's real headers to 5,000: the 2×spacing minimum-difficulty rule below KGW,
    /// the last-other-bits walk-back, and the 2 h rule in DGW's era.
    #[replays(["a1a-testnet-min-difficulty"])]
    fn replay_a1a_testnet_min_difficulty() {
        assert_eq!(replay("a1a-testnet-min-difficulty"), 7);
    }
}

scenario_test! {
    /// A-1a on mainnet's real headers to 300 with a seeded lock at R250: `bad-chainlock` (kept as
    /// CONFLICT), `duplicate`, `bad-prevblk-chainlock`, and acceptance above the lock.
    #[replays(["a1a-mainnet-stale-branch-chainlock"])]
    fn replay_a1a_mainnet_stale_branch_chainlock() {
        assert_eq!(replay("a1a-mainnet-stale-branch-chainlock"), 5);
    }
}

scenario_test! {
    /// A-18's header half: C6..C30 accepted on a branch whose C7 is an invalid block, then, after
    /// its `connect` fails C7 and marks its descendants (`InvalidChainFound`), C31 on C30 is
    /// `bad-prevblk`. The connect is CH1-node's block validation (D-C6-7): its view is injected.
    #[replays(["a18-invalid-chain-found"])]
    fn replay_a18_invalid_chain_found() {
        assert_eq!(replay("a18-invalid-chain-found"), 2);
    }
}

scenario_test! {
    /// A-18's header half: B96..B110 accepted beside the active A100. The `invalidateblock` and
    /// `reconsiderblock` walk-back after it is CH1-node's (D-C6-7): its views are injected.
    #[replays(["a18-invalidate-walkback"])]
    fn replay_a18_invalidate_walkback() {
        assert_eq!(replay("a18-invalidate-walkback"), 1);
    }
}

scenario_test! {
    /// HX1-X1's header half: B86..B110 accepted beside the tip A90, then B111 is `bad-prevblk` on
    /// B110's FAILED_CHILD after the failed connect. The lock's enforcement over header-only A100
    /// and the connects it judges are CH1-node's (HX1 X5): their views are injected.
    #[replays(["hx1-bad-chainlock-then-connect"])]
    fn replay_hx1_bad_chainlock_then_connect() {
        assert_eq!(replay("hx1-bad-chainlock-then-connect"), 3);
    }
}

scenario_test! {
    /// HX1-X3's header half: B and C headers accepted, then the reload's index against the
    /// pre-reload one and Core's statuses and best header. The blocks' arrival orders are CH1-node's block data (HX1 X5).
    #[replays(["hx1-dump-orders"])]
    fn replay_hx1_dump_orders() {
        assert_eq!(replay("hx1-dump-orders"), 2);
    }
}

scenario_test! {
    /// HX1-X4: C4, whose parent descends from two invalidated blocks, fails `high-hash` in
    /// CheckBlockHeader before the failed-blocks walk, and nothing is written.
    #[replays(["hx1-failed-walk-early-reject"])]
    fn replay_hx1_failed_walk_early_reject() {
        assert_eq!(replay("hx1-failed-walk-early-reject"), 2);
    }
}

scenario_test! {
    /// HX1-X2's header half: header-only C11 (more work) accepted beside the equal-work A10/B10
    /// tips. `preciousblock` is CH1-node's RPC (HX1 X5): its views are injected.
    #[replays(["hx1-precious"])]
    fn replay_hx1_precious() {
        assert_eq!(replay("hx1-precious"), 1);
    }
}

scenario_test! {
    /// HX1-X1's header half: header-only F11 accepted on the tip. The `getblocktemplate` proposals
    /// and the lock on F11 they are judged against are CH1-node's (HX1 X5): their views are
    /// injected.
    #[replays(["hx1-propose-check"])]
    fn replay_hx1_propose_check() {
        assert_eq!(replay("hx1-propose-check"), 1);
    }
}

scenario_test! {
    /// A-19's acceptance half: the first of two equal-work headers stays the best header. After the
    /// reload Core reports an `address_tie`, so only membership in the tied set is checked (D9).
    #[replays(["a19-first-seen-tie"])]
    fn replay_a19_first_seen_tie() {
        assert_eq!(replay("a19-first-seen-tie"), 3);
    }
}

scenario_test! {
    /// A-38's harness half on a node chain: a stale branch forking 30 below the tip, `time-too-old`
    /// at its MTP and accepted one second later.
    #[replays(["a38-deep-fork-time-too-old"])]
    fn replay_a38_deep_fork_time_too_old() {
        assert_eq!(replay("a38-deep-fork-time-too-old"), 4);
    }
}

scenario_test! {
    /// A-38's harness half on SPV chains from genesis: at w = 10 the fork's parent A70 and its
    /// ancestors are cold (read from their records), and Core's verdicts hold, `time-too-old` at
    /// the MTP included.
    #[replays(["a38-deep-fork-time-too-old"])]
    fn replay_a38_deep_fork_time_too_old_spv() {
        let s = load_scenario("a38-deep-fork-time-too-old");
        for w in [10_000, 10] {
            let n = replay_spv(&s, &format!("a38, w = {w}"), 0, |p, svc, _| {
                spv_tests::genesis_with(p, w, svc)
            });
            assert_eq!(n, 4, "w = {w}: every submission compared");
        }
    }
}

scenario_test! {
    #[replays(["smoke-reload"])]
    fn replay_smoke_reload() {
        assert_eq!(replay("smoke-reload"), 1);
    }
}

scenario_test! {
    #[replays(["a24-checkpoint"])]
    fn replay_a24_checkpoint() {
        assert_eq!(replay("a24-checkpoint"), 3);
    }
}

scenario_test! {
    #[replays(["a28-pnbh-index-state"])]
    fn replay_a28_pnbh_index_state() {
        assert_eq!(replay("a28-pnbh-index-state"), 8);
    }
}

scenario_test! {
    /// A-2: a2's battery (Core's verdicts above A34200) on SPV chains anchored at A34164, DGW's
    /// lowest anchor, and at A34200, whose rule context is then the pack. Each `pnbh` is one SPV
    /// step (accept, commit, tip, stage 5), so at w = 10 the context's lower entries are read cold.
    /// Compared: ok, class, reason, debug, `last_accepted` and the best header.
    #[replays(["a2-pnbh-rules-anchored"])]
    fn replay_a2_pnbh_rules_anchored() {
        let s = load_scenario("a2-pnbh-rules-anchored");
        for anchor in [34_164, 34_200] {
            for w in [10_000, 1_000, 10] {
                let at = format!("a2 anchored at A{anchor}, w = {w}");
                let n = replay_spv(&s, &at, anchor, |p, svc, headers| {
                    let a = |h: u32| headers[&format!("A{h}")];
                    let genesis = Header::new(dash_consensus::params::genesis_block(p).header);
                    let work = std::iter::once(genesis)
                        .chain((1..=anchor).map(a))
                        .fold(Work::ZERO, |w, h| w + block_proof(CompactTarget::new(h.raw().bits)));
                    let pack = AnchorPack {
                        height: anchor,
                        hash: a(anchor).hash(),
                        chain_work: work,
                        headers: std::array::from_fn(|k| *a(anchor - 24 + k as u32).raw()),
                    };
                    spv_tests::anchored_with(p, &pack, w, svc)
                });
                assert_eq!(n, 11, "anchor A{anchor}, w = {w}: every submission compared");
            }
        }
    }
}

/// Replays a regtest scenario's header submissions on the SPV chain `make` builds (from its
/// parameters, the services and the headers built so far, at the first submission), one SPV
/// step (accept, commit, tip, stage 5) per submission; returns the submissions compared.
/// `A<n>` below `anchor` (0 from genesis) is left out of a batch; a `connect` build is accepted
/// as headers and must be taken whole. Compared: ok, class, reason, debug, `last_accepted` and
/// the best header.
fn replay_spv(
    s: &Scenario,
    name: &str,
    anchor: u32,
    make: impl FnOnce(&Arc<ChainSetup>, Services, &HashMap<String, Header>) -> (Spv, MemHeaderStore),
) -> usize {
    let (vector, p) = (&s.vector, &s.params);
    assert_eq!(vector["network"], "regtest", "{name}");
    let genesis = Header::new(dash_consensus::params::genesis_block(p).header);
    let mut now = i64::from(genesis.time()) + 10_000_000;
    let mut headers: HashMap<String, Header> = HashMap::new();
    let mut labels: HashMap<BlockHash, String> = HashMap::from([(genesis.hash(), "G".to_owned())]);
    let mut make = Some(make);
    let mut chain = None;
    let clock = Arc::new(MockClock::at(now));
    let mut compared = 0;
    for (i, step) in vector["steps"].as_array().expect("steps").iter().enumerate().skip(1) {
        let at = format!("{name}, step {i}");
        let (op, result) = (&step["op"], &step["result"]);
        let (batch, expect) = match op["op"].as_str().expect("op name") {
            "build" => {
                let built = expand(&Value::from(vec![result["built"].clone()]));
                for l in &built {
                    let bytes = hex(result["headers"][l].as_str().expect("header hex"));
                    let header = Header::new(BlockHeader::decode(&mut &bytes[..]).expect("80 B"));
                    if op.get("time").is_none() {
                        now = now.max(i64::from(header.time()) + 60);
                    }
                    labels.insert(header.hash(), l.clone());
                    headers.insert(l.clone(), header);
                }
                match op["submit"].as_str() {
                    Some("headers") => (built, Some(&result["pnbh"])),
                    Some("connect") => (built, None),
                    _ => continue,
                }
            }
            "pnbh" => (expand(&op["headers"]), Some(result)),
            other => panic!("{at}: no {other} in an SPV replay"),
        };
        let (chain, store) = chain.get_or_insert_with(|| {
            make.take().expect("built once")(p, services_with(p, clock.clone()), &headers)
        });
        // The first build's headers below the anchor are the pack's, or below its floor; the
        // anchor itself is resubmitted, so A34200's step 1 is a known header.
        let below = |l: &String| matches!(l.strip_prefix('A').map(str::parse::<u32>), Some(Ok(n)) if n < anchor);
        let batch: Vec<Header> = batch.iter().filter(|l| !below(l)).map(|l| headers[l]).collect();
        clock.set_mocktime(now);
        let mut r = None;
        for part in batch.chunks(2_000) {
            r = Some(spv_tests::step(chain, store, part));
        }
        let r = r.expect("a batch");
        let Some(expect) = expect else {
            assert_eq!(r.stop, None, "{at}: a connected block's header");
            continue;
        };
        let (class, reason, debug) = match &r.stop {
            None => (R::Unset, "", ""),
            Some(HeaderStop::Invalid {
                reject,
                ..
            }) => (reject.result(), reject.reject_reason(), reject.debug_message()),
            Some(stop) => panic!("{at}: {stop:?}"),
        };
        let state = &expect["state"];
        assert_eq!(expect["ok"], r.stop.is_none(), "{at}: ok");
        assert_eq!(state["class"], class_name(class), "{at}: class");
        assert_eq!(state["reason"], reason, "{at}: reason");
        assert_eq!(state["debug"], debug, "{at}: debug");
        let label_of = |id: BlockId| {
            let hash = chain.reader().try_entry(id).expect("read").expect("held").hash;
            labels[&hash].clone()
        };
        let last = expect["last_accepted"].as_str();
        assert_eq!(r.last_accepted.map(label_of).as_deref(), last, "{at}: last_accepted");
        let best = step["view"]["best_header"].as_str().expect("best header");
        assert_eq!(label_of(chain.best_header()), best, "{at}: best header");
        compared += 1;
    }
    compared
}

scenario_test! {
    #[replays(["smoke-pnbh"])]
    fn replay_smoke_pnbh() {
        assert_eq!(replay("smoke-pnbh"), 2);
    }
}

// ---- `headers` mode: one real batch after Core's preload of the dump below it ---------------

scenario_test! {
    /// `pnbh-mainnet-dgw`: H-M's 2,000,000..2,002,000 on an SPV chain anchored at 1,999,999 with
    /// the reference node's chain work there (`benches/data/README.md`), so the best header's chain
    /// work is compared with Core's too.
    #[replays(["pnbh-mainnet-dgw"])]
    fn replay_pnbh_mainnet_dgw() {
        let v = load("pnbh-mainnet-dgw");
        let slice = slice_headers("benches/data/mainnet-1999950-2002000.bin");
        let (first, from) = (1_999_950, 2_000_000);
        let work = Work::from_u128(0x8f6a_7c45_9d4e_1aea_a76b);
        replay_headers_mode(
            &v,
            Network::Main,
            &slice[(from - 25 - first) as usize..],
            work,
            true,
        );
    }
}

scenario_test! {
    /// `pnbh-testnet`: H-T's 1,000,000..1,002,000 on an SPV chain anchored at 999,999. No reference
    /// work was recorded there, so the anchor's is Core's final chain work less the batch's, and
    /// the chain work is not compared.
    #[replays(["pnbh-testnet"])]
    fn replay_pnbh_testnet() {
        let v = load("pnbh-testnet");
        let slice = slice_headers("tests/data/header-slices/testnet-999975-1002000.bin");
        let batch_work = slice[25..].iter().fold(Work::ZERO, |w, h| w + block_proof(bits(h)));
        let work = work_hex(&v["best_header_chainwork"]).wrapping_sub(batch_work);
        replay_headers_mode(&v, Network::Testnet3, &slice, work, false);
    }
}

scenario_test! {
    /// `pnbh-mainnet-kgw`: H-M's 20,000..22,000 on a node chain holding H-M's 15,900..19,999 above
    /// synthetic entries (KGW reads back to 15,967; `benches/data/README.md`). The synthetic work
    /// below makes the chain work incomparable; the verdict, the last accepted and the best header
    /// are compared.
    #[replays(["pnbh-mainnet-kgw"])]
    fn replay_pnbh_mainnet_kgw() {
        let v = load("pnbh-mainnet-kgw");
        let slice = slice_headers("benches/data/mainnet-15900-22000.bin");
        let (first, from) = (15_900u32, 20_000u32);
        assert_eq!((v["from"].as_u64(), v["count"].as_u64()), (Some(20_000), Some(2_000)));
        let p = params(Network::Main);
        let svc = services_at(&p, 2_000_000_000);
        let mut chain =
            HeaderChain::<NoStore>::new_genesis(p.clone(), ChainConfig::node(&p), svc, NoStore)
                .expect("genesis");
        let mut prev = Header::new(dash_consensus::params::genesis_block(&p).header).hash();
        let first_time = slice[0].time();
        for h in 1..first {
            let header = BlockHeader {
                version: 1,
                prev_hash: prev,
                time: first_time - 150 * (first - h),
                bits: slice[0].raw().bits,
                ..BlockHeader::default()
            };
            prev = fake_hash(u64::from(h));
            chain.add_to_block_index(&header, &prev, Status(Status::VALID_TREE)).expect("added");
        }
        for (k, h) in slice[..(from - first) as usize].iter().enumerate() {
            let mut header = *h.raw();
            // The lowest real header's parent is the synthetic one (`load` and this insert do not
            // hash).
            if k == 0 {
                header.prev_hash = prev;
            }
            prev = h.hash();
            chain.add_to_block_index(&header, &prev, Status(Status::VALID_TREE)).expect("added");
        }
        chain.init_active_tip(BlockId(0)).expect("Core LoadChainTip");
        let batch_headers = &slice[(from - first) as usize..(from - first + 2_000) as usize];
        let r = chain.accept(batch(batch_headers), None).expect("accept");
        let hash = |id: BlockId| chain.shared.arena.imm(id, |e| e.hash).expect("resident");
        let height = chain.shared.arena.imm(chain.best_header(), |e| e.height).expect("resident");
        let best = (hash(chain.best_header()), height);
        compare_headers_mode(&v, &r, r.last_accepted.map(hash), best);
    }
}

/// Heights `first..` of a committed slice of H-M or H-T.
fn slice_headers(rel: &str) -> Vec<Header> {
    let bytes = crate_file(rel);
    assert!(bytes.len().is_multiple_of(80), "{rel}: a partial header");
    let decode = |h: &[u8; 80]| Header::new(BlockHeader::decode(&mut &h[..]).expect("80 B"));
    bytes.as_chunks::<80>().0.iter().map(decode).collect()
}

fn bits(h: &Header) -> CompactTarget {
    CompactTarget::new(h.raw().bits)
}

/// Core's `chainwork` hex.
fn work_hex(v: &Value) -> Work {
    let bytes: [u8; 32] = hex(v.as_str().expect("hex")).try_into().expect("32 bytes");
    Work::from_bendian(bytes)
}

/// Core's display hash (`GetHex`, reversed) as ours.
fn rpc_hash(v: &Value) -> BlockHash {
    let mut bytes: [u8; 32] = hex(v.as_str().expect("hex")).try_into().expect("32 bytes");
    bytes.reverse();
    BlockHash::from(bytes)
}

/// A `headers`-mode `pnbh`: the 25 headers of `slice` are the anchor pack (with `work` at its
/// top), and the next `count` the batch, taken in one SPV step at w = 10,000.
fn replay_headers_mode(v: &Value, net: Network, slice: &[Header], work: Work, cmp_work: bool) {
    let p = params(net);
    let count = v["count"].as_u64().expect("count") as usize;
    let anchor = u32::try_from(v["from"].as_u64().expect("from") - 1).expect("a height");
    let pack = AnchorPack {
        height: anchor,
        hash: slice[24].hash(),
        chain_work: work,
        headers: std::array::from_fn(|k| *slice[k].raw()),
    };
    let svc = services_at(&p, 2_000_000_000);
    let (mut chain, mut store) = spv_tests::anchored_with(&p, &pack, 10_000, svc);
    let r = spv_tests::step(&mut chain, &mut store, &slice[25..25 + count]);
    let entry = |id: BlockId| chain.reader().try_entry(id).expect("read").expect("held");
    let best = entry(chain.best_header());
    compare_headers_mode(v, &r, r.last_accepted.map(|id| entry(id).hash), (best.hash, best.height));
    if cmp_work {
        assert_eq!(chain.work(chain.best_header()), work_hex(&v["best_header_chainwork"]));
    }
}

/// Compares a `headers`-mode result: ok, the state, the last accepted and the best header (hash
/// and height).
fn compare_headers_mode(
    v: &Value,
    r: &HeadersResult,
    last: Option<BlockHash>,
    best: (BlockHash, u32),
) {
    let (class, reason, debug) = match &r.stop {
        None => (R::Unset, "", ""),
        Some(HeaderStop::Invalid {
            reject,
            ..
        }) => (reject.result(), reject.reject_reason(), reject.debug_message()),
        Some(stop) => panic!("{stop:?}"),
    };
    let state = &v["state"];
    assert_eq!(v["ok"], r.stop.is_none(), "ok");
    assert_eq!(state["valid"], r.stop.is_none(), "valid");
    assert_eq!(state["class"], class_name(class), "class");
    assert_eq!(state["reason"], reason, "reason");
    assert_eq!(state["debug"], debug, "debug");
    assert_eq!(last, Some(rpc_hash(&v["last_accepted"])), "last accepted");
    assert_eq!(best.0, rpc_hash(&v["best_header"]), "best header");
    assert_eq!(Some(u64::from(best.1)), v["best_header_height"].as_u64(), "best header height");
}

scenario_test! {
    /// Core's locator heights at 64 mainnet tips (`locator-mainnet`, harness mode `locator`).
    #[replays(["locator-mainnet"])]
    fn locator_heights_match_core_on_mainnet() {
        let v = load("locator-mainnet");
        let rows = v["rows"].as_array().expect("rows");
        assert_eq!(rows.len(), 64);
        for row in rows {
            let height = u32::try_from(row["height"].as_u64().expect("height")).expect("a height");
            let core: Vec<u32> =
                serde_json::from_value(row["locator_heights"].clone()).expect("heights");
            assert_eq!(crate::walk::locator_heights(height), core, "tip {height}");
        }
    }
}

scenario_test! {
    /// The record codec against Core's `CDiskBlockIndex` bytes (`smoke-diskindex`): our value is
    /// Core's record without the client-version prefix and the inline hash.
    #[replays(["smoke-diskindex"])]
    fn record_codec_matches_core_disk_index() {
        let s = load_scenario("smoke-diskindex");
        let mut checked = 0;
        for step in s.vector["steps"].as_array().expect("steps") {
            if step["op"]["op"] != "diskindex" {
                continue;
            }
            for (label, h) in step["result"].as_object().expect("diskindex result is a map") {
                let core = hex(h.as_str().expect("hex"));
                let ours = core_record(&core);
                let record = crate::record::BlockIndexRecord::decode(&ours)
                    .unwrap_or_else(|e| panic!("{label}: {e}"));
                assert_eq!(record.to_bytes(), ours, "{label}: re-encodes byte for byte");
                let (hash, header) = core[core.len() - 112..].split_at(32);
                assert_eq!(dash_consensus::primitives::header_bytes(&record.header)[..], *header);
                assert_eq!(Header::new(record.header).hash().as_bytes()[..], *hash, "{label}");
                checked += 1;
            }
        }
        assert_eq!(checked, 8, "G, A1..A5, B4..B5");
    }
}

/// The vector file `name` (`{provenance, vector}`, gzipped JSON), from
/// `dash-chain/tests/data/core-scenarios` or `SB4_CORE_SCENARIOS_DIR`. The running test must be
/// registered for it (`coverage.rs`).
fn load_file(name: &str) -> Value {
    coverage::check_load(name);
    let path = coverage::dir().join(format!("{name}{}", coverage::EXT));
    let file = std::fs::File::open(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (set SB4_CORE_SCENARIOS_DIR)", path.display()));
    let value: Value =
        serde_json::from_reader(flate2::read::GzDecoder::new(std::io::BufReader::new(file)))
            .expect("vector JSON");
    let vector = &value["vector"];
    match vector.get("name") {
        Some(n) => assert_eq!(n, name, "{}", path.display()),
        // A `headers`-mode vector is named after its mode.
        None => {
            let mode = vector["mode"].as_str().expect("a scenario name or a headers mode");
            assert!(name.starts_with(&format!("{mode}-")), "{}: mode {mode}", path.display());
        }
    }
    value
}

/// The vector `name` (the file's `vector`) of a mode with no scenario file (`headers`, `locator`).
fn load(name: &str) -> Value {
    load_file(name)["vector"].take()
}

/// `bytes`' SHA-256 in hex, as the vectors' provenance records it.
fn sha256_hex(bytes: &[u8]) -> String {
    dash_consensus::hash::sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A file under the crate's directory.
fn crate_file(rel: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The harness's scenario file for vector `name`, checked against the vector's provenance (the
/// file Core ran).
fn scenario(name: &str, provenance: &Value) -> Value {
    let rel = format!("contrib/node/sb4-core-scenarios/scenarios/{name}.json");
    let bytes = crate_file(&format!("../{rel}"));
    assert_eq!(
        provenance["inputs_sha256"][&rel],
        sha256_hex(&bytes),
        "{rel}: not the file Core ran"
    );
    serde_json::from_slice(&bytes).expect("scenario JSON")
}

/// A vector with the scenario Core ran it from.
struct Scenario {
    /// The vector file's `vector`.
    vector: Value,
    /// The vector file's `provenance`.
    provenance: Value,
    /// The harness's scenario file, checked against the provenance.
    scenario: Value,
    /// The chain parameters Core ran it with: its network and its arguments.
    params: Arc<ChainSetup>,
}

/// The vector `name` with its scenario (`scenario`, `scenario_params`).
fn load_scenario(name: &str) -> Scenario {
    let mut file = load_file(name);
    let (vector, provenance) = (file["vector"].take(), file["provenance"].take());
    let scenario = scenario(name, &provenance);
    assert_eq!(scenario["network"], vector["network"], "{name}");
    let params = scenario_params(&scenario);
    Scenario {
        vector,
        provenance,
        scenario,
        params,
    }
}

/// The chain parameters Core ran `scenario` with: its network and its arguments.
fn scenario_params(scenario: &Value) -> Arc<ChainSetup> {
    let network = match scenario["network"].as_str().expect("network") {
        "regtest" => Network::Regtest,
        "devnet" => Network::Devnet,
        "test" => Network::Testnet3,
        "main" => Network::Main,
        other => panic!("network {other}"),
    };
    let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
    for arg in scenario.get("args").and_then(Value::as_array).into_iter().flatten() {
        let (option, value) = arg.as_str().expect("an argument").split_once('=').expect("-x=v");
        let option = ChainOption::from_name(option).unwrap_or_else(|| panic!("{option}"));
        builder = builder.arg(option, value);
    }
    Arc::new(builder.build().expect("Core's parameters"))
}

/// Replays vector `name`; returns the number of `pnbh` steps compared.
fn replay(name: &str) -> usize {
    let s = load_scenario(name);
    let (vector, provenance, scenario) = (&s.vector, &s.provenance, &s.scenario);
    let mut r = Replay::on(s.params.clone());
    let (mut compared, mut skipped, mut injected) = (0, BTreeSet::new(), BTreeSet::new());
    let owed = coverage::owed(name);
    for (i, step) in vector["steps"].as_array().expect("steps").iter().enumerate() {
        let at = format!("{name} step {i}");
        let (op, result, view) = (&step["op"], &step["result"], &step["view"]);
        // The headers of a `pnbh` this step makes, with Core's result.
        let mut batch = None;
        match op.as_str().or_else(|| op["op"].as_str()).expect("op name") {
            "setup" => r.setup(scenario, provenance, result, &at),
            "build" => {
                let labels = r.build(op, result);
                match op["submit"].as_str().unwrap_or("none") {
                    "headers" => batch = Some((labels, &result["pnbh"])),
                    "connect" => {
                        assert_eq!(result["connect"], Value::Array(vec![]), "{at}: Core's fails");
                        r.connect(&labels, &at);
                        // An off-tip block a reorg would connect is beyond `connect`'s model.
                        let tip = r.chain.private_tip().expect("a tip").0.id;
                        assert_eq!(r.label(tip), label(&view["tip"]), "{at}: tip");
                    }
                    "none" => {}
                    other => panic!("{at}: submit {other}"),
                }
            }
            "pnbh" => batch = Some((expand(&op["headers"]), result)),
            "checkpoint" => r.checkpoint(op["block"].as_str().expect("block")),
            "reload" => {
                r.reload(op["checkpoints"].as_bool().unwrap_or(true), view, &at);
                let best = r.label(r.chain.best_header());
                assert!(best_header_set(&view["best_header"]).contains(&best), "{at}: {best}");
            }
            // The harness steps the clock a second before each spork message.
            "spork19" => r.now += 1,
            "mocktime" => r.now += op["advance"].as_i64().expect("advance"),
            // Injected below from Core's view; owed to the row's owner.
            other => {
                assert!(owed.contains(other), "{at}: op {other} is neither replayed nor OWED");
                injected.insert(other);
                if other == "connect" {
                    // Core accepts every block's header; a rejected one would fail later blocks.
                    assert_eq!(*result, Value::Array(vec![]), "{at}: Core's fails");
                    let changes = status_changes(view);
                    for l in r.accept_unknown(&expand(&op["blocks"]), &at) {
                        assert!(changes.contains_key(&l), "{at}: {l} is not in Core's index");
                    }
                }
            }
        }
        let Some((labels, result)) = batch else {
            r.inject(view);
            continue;
        };
        if SKIPPED.iter().any(|&(v, s, _)| (v, s) == (name, i)) {
            r.pnbh(&labels, None, &at);
            r.inject(view);
            skipped.insert(i);
        } else {
            r.pnbh(&labels, Some((result, view)), &at);
            compared += 1;
        }
    }
    let listed: BTreeSet<usize> =
        SKIPPED.iter().filter(|&&(v, ..)| v == name).map(|&(_, s, _)| s).collect();
    assert_eq!(skipped, listed, "{name}: SKIPPED lists exactly the skipped pnbh steps");
    assert_eq!(injected, owed, "{name}: OWED lists exactly the injected ops");
    compared
}

/// The replay's state: the chain and its services, the harness clock, and each label's header.
struct Replay {
    chain: HeaderChain,
    params: Arc<ChainSetup>,
    clock: Arc<MockClock>,
    sporks: Arc<StaticSporks>,
    /// The harness's mock clock (README "Time"), set on the chain's clock at each `pnbh`.
    now: i64,
    headers: HashMap<String, Header>,
    labels: HashMap<BlockHash, String>,
}

impl Replay {
    /// A node chain at genesis on `params`.
    fn on(params: Arc<ChainSetup>) -> Replay {
        let genesis = Header::new(dash_consensus::params::genesis_block(&params).header);
        // Regtest's and devnet's clock starts 10,000,000 s after genesis; a preload moves it.
        let now = i64::from(genesis.time()) + 10_000_000;
        let clock = Arc::new(MockClock::at(now));
        let sporks = Arc::new(StaticSporks::for_network(&params, None));
        let svc = Services {
            clock: clock.clone(),
            sporks: sporks.clone(),
            fail: Arc::new(Latch::default()),
        };
        let cfg = ChainConfig::node(&params);
        let mut chain = HeaderChain::new_genesis(params.clone(), cfg, svc, NoStore).expect("node");
        chain.init_active_tip(BlockId(0)).expect("Core LoadChainTip");
        let mut r = Replay {
            chain,
            params,
            clock,
            sporks,
            now,
            headers: HashMap::new(),
            labels: HashMap::new(),
        };
        r.add_label("G", genesis);
        r
    }

    /// The harness's setup beyond genesis: a devnet's block 1 (`DG`, connected, as Core's
    /// `LoadGenesisBlock` loads it), or the scenario's preload (`R1..Rn`, accepted as headers,
    /// the clock an hour after the latest).
    fn setup(&mut self, scenario: &Value, provenance: &Value, result: &Value, at: &str) {
        if let Some(dg) = dash_consensus::params::devnet_genesis_block(&self.params) {
            self.add_label("DG", Header::new(dg.header));
            self.connect(&["DG".to_owned()], at);
        }
        let Some(preload) = scenario.get("preload") else {
            assert!(result.is_null(), "{at}: no preload");
            return;
        };
        let file = preload["file"].as_str().expect("a dump");
        let count = preload["count"].as_u64().expect("a count");
        let net = file.strip_suffix(".bin").expect("a dump");
        let bytes = crate_file(&format!("tests/data/header-slices/{net}-0-{count}.bin"));
        let key = format!("{file}[:{count} headers]");
        assert_eq!(provenance["inputs_sha256"][&key], sha256_hex(&bytes), "{at}: {key}");
        let headers: Vec<Header> = bytes
            .as_chunks::<80>()
            .0
            .iter()
            .map(|h| Header::new(BlockHeader::decode(&mut &h[..]).expect("80 B")))
            .collect();
        assert_eq!(headers.len() as u64, count, "{at}");
        assert_eq!(headers[0].hash(), self.hash("G"), "{at}: the preload starts at genesis");
        let latest = headers.iter().map(|h| i64::from(h.time())).max().expect("headers");
        self.now = latest + 3_600;
        let labels: Vec<String> = (1..headers.len()).map(|h| format!("R{h}")).collect();
        for (l, h) in labels.iter().zip(&headers[1..]) {
            self.add_label(l, *h);
        }
        assert_eq!(result["preloaded"], format!("R1..R{}", count - 1), "{at}");
        self.clock.set_mocktime(self.now);
        for part in labels.chunks(2_000) {
            let r = self.chain.accept(self.batch(part), None).expect("accept");
            assert_eq!(r.stop, None, "{at}: the preload");
        }
    }

    fn add_label(&mut self, label: &str, header: Header) {
        self.labels.insert(header.hash(), label.to_owned());
        assert!(self.headers.insert(label.to_owned(), header).is_none(), "{label} reused");
    }

    fn hash(&self, label: &str) -> BlockHash {
        self.headers.get(label).unwrap_or_else(|| panic!("no header {label}")).hash()
    }

    fn id(&self, label: &str) -> BlockId {
        self.chain.shared.lookup(&self.hash(label)).unwrap_or_else(|| panic!("{label} not indexed"))
    }

    fn label(&self, id: BlockId) -> String {
        let hash = self.chain.shared.arena.imm(id, |e| e.hash).expect("resident");
        self.labels.get(&hash).cloned().unwrap_or_else(|| panic!("no label for {hash:?}"))
    }

    /// Records `build`'s headers; returns the labels in build order. A block built without
    /// `time` keeps the harness clock a minute ahead of it.
    fn build(&mut self, op: &Value, result: &Value) -> Vec<String> {
        let labels = expand(&Value::from(vec![result["built"].clone()]));
        for l in &labels {
            let bytes = hex(result["headers"][l].as_str().expect("header hex"));
            let raw = BlockHeader::decode(&mut &bytes[..]).expect("an 80-byte header");
            let header = Header::new(raw);
            // The harness reports the bits it built with (`"bits": "next"` is Core's
            // GetNextWorkRequired); they must be the header's.
            if let Some(b) = result.get("bits").and_then(|b| b.get(l.as_str())) {
                assert_eq!(b.as_str(), Some(format!("{:08x}", header.raw().bits).as_str()), "{l}");
            }
            if op.get("time").is_none() {
                self.now = self.now.max(i64::from(header.time()) + 60);
            }
            self.add_label(l, header);
        }
        labels
    }

    /// `ProcessNewBlock` per block as far as the header index sees it: the header is accepted,
    /// and a block that extends the tip is connected. An off-tip block is left to the step's
    /// injected view, which sets the tip and statuses: Core stores it without connecting when it
    /// adds no work (HX1's `hx1-precious` builds B10 beside the tip A10). The caller checks the
    /// tip against Core's, so a reorg by work fails there.
    fn connect(&mut self, labels: &[String], at: &str) {
        for l in labels {
            self.clock.set_mocktime(self.now);
            let r = self.chain.accept(self.batch(std::slice::from_ref(l)), None).expect("accept");
            assert_eq!(r.stop, None, "{at}: {l}");
            let id = self.id(l);
            let tip = self.chain.private_tip().expect("a tip").0.id;
            if self.chain.shared.arena.imm(id, |e| e.prev).flatten() == Some(tip) {
                self.chain.connect_tip(id).expect("prev is the tip");
            }
        }
    }

    /// The headers an injected op's blocks bring (`ProcessNewBlock` accepts each block's header
    /// first), in the op's order, so its view can name them (`hx1-dump-orders`). Returns the
    /// labels it accepted.
    fn accept_unknown(&mut self, labels: &[String], at: &str) -> Vec<String> {
        let mut accepted = Vec::new();
        for l in labels {
            if self.chain.shared.lookup(&self.hash(l)).is_some() {
                continue;
            }
            self.clock.set_mocktime(self.now);
            let r = self.chain.accept(self.batch(std::slice::from_ref(l)), None).expect("accept");
            assert_eq!(r.stop, None, "{at}: {l}");
            accepted.push(l.clone());
        }
        accepted
    }

    fn batch(&self, labels: &[String]) -> HeaderBatch {
        let headers: Vec<Header> = labels.iter().map(|l| self.headers[l]).collect();
        batch(&headers)
    }

    /// One `ProcessNewBlockHeaders` batch; with `expect`, compared with Core's result and view.
    fn pnbh(&mut self, labels: &[String], expect: Option<(&Value, &Value)>, at: &str) {
        // The block tree before the batch, when its writes are compared.
        let disk = match expect {
            Some(_) => self.records(),
            None => {
                self.chain.take_dirty();
                HashMap::new()
            }
        };
        let before = self.statuses();
        self.clock.set_mocktime(self.now);
        let r = self.chain.accept(self.batch(labels), None).expect("no internal failure");
        let Some((result, view)) = expect else {
            return;
        };
        let (class, reason, debug) = match &r.stop {
            None => (R::Unset, "", ""),
            Some(HeaderStop::Invalid {
                reject,
                ..
            }) => (reject.result(), reject.reject_reason(), reject.debug_message()),
            Some(HeaderStop::BelowAnchor {
                ..
            }) => panic!("{at}: a node chain has no anchor"),
        };
        let state = &result["state"];
        assert_eq!(result["ok"], r.stop.is_none(), "{at}: ok");
        assert_eq!(state["valid"], r.stop.is_none(), "{at}: valid");
        assert_eq!(state["class"], class_name(class), "{at}: class");
        assert_eq!(state["reason"], reason, "{at}: reason");
        assert_eq!(state["debug"], debug, "{at}: debug");
        let last = result["last_accepted"].as_str();
        assert_eq!(r.last_accepted, last.map(|l| self.id(l)), "{at}: last_accepted");

        let best = self.label(self.chain.best_header());
        assert!(best_header_set(&view["best_header"]).contains(&best), "{at}: best header {best}");
        let tip = self.chain.private_tip().expect("a tip").0.id;
        assert_eq!(self.label(tip), label(&view["tip"]), "{at}: tip");
        let failed: BTreeSet<String> = self.chain.failed.iter().map(|id| self.label(id)).collect();
        assert_eq!(failed, labels_of(&view["failed_blocks"]), "{at}: failed blocks");
        let floor = self.chain.cl_floor().map(|id| self.label(id));
        assert_eq!(floor.as_deref(), view["cl"]["floor"].as_str(), "{at}: ChainLock floor");

        let after = self.statuses();
        let changed: BTreeMap<String, u32> =
            after.into_iter().filter(|(l, s)| before.get(l) != Some(s)).collect();
        // A record rewritten with the same bytes is not a change (`harness.cpp`'s `Pnbh`).
        let written: BTreeMap<String, Vec<u8>> = self
            .chain
            .take_dirty()
            .into_iter()
            .filter(|w| disk.get(&w.hash) != Some(&w.bytes))
            .map(|w| (self.labels[&w.hash].clone(), w.bytes))
            .collect();
        assert_eq!(changed, status_changes(view), "{at}: status changes");
        let core: BTreeMap<String, Vec<u8>> = result["records_written"]
            .as_object()
            .expect("records_written")
            .iter()
            .map(|(l, h)| (l.clone(), core_record(&hex(h.as_str().expect("hex")))))
            .collect();
        assert_eq!(written, core, "{at}: records written");
    }

    /// Every entry's `nStatus`, by label.
    fn statuses(&self) -> BTreeMap<String, u32> {
        let chain = &self.chain;
        chain
            .ids()
            .map(|id| (self.label(id), chain.writer_status(id).expect("known").bits()))
            .collect()
    }

    /// The harness's `checkpoint` op: `mapCheckpoints[height] = hash` in the parameters. The
    /// chain's map and `GetLastCheckpoint` cache follow, as Core reads both live.
    fn checkpoint(&mut self, label: &str) {
        let (hash, id) = (self.hash(label), self.id(label));
        let height = self.chain.shared.arena.imm(id, |e| e.height).expect("resident");
        let mut p = (*self.params).clone();
        p.checkpoints.retain(|(h, _)| h.to_u32() != height);
        p.checkpoints.push((height.into(), dash_num::Hash256::from(*hash.as_bytes())));
        self.params = Arc::new(p);
        self.chain.checkpoints.retain(|_, &mut k| k != height);
        self.chain.checkpoints.insert(hash, height);
        self.chain.note_checkpoint(&hash, height);
    }

    /// The harness's `reload`: the index from its records (every entry's), with no tip until
    /// the view's is injected. Each entry reloads as written (its record, height, parent and
    /// chain work), but for the statuses the reload changes in Core (`LoadBlockIndex` marks a
    /// failed entry's children), which must be the view's.
    fn reload(&mut self, checkpoints: bool, view: &Value, at: &str) {
        let changes = status_changes(view);
        let mut expect = self.index();
        for (l, s) in &changes {
            expect.get_mut(l).unwrap_or_else(|| panic!("{at}: {l} not indexed")).0 = *s;
        }
        let written = self.records();
        self.reload_with(written.clone().into_iter(), checkpoints);
        assert_eq!(self.index(), expect, "{at}: the reloaded index");
        let read = self.records();
        let unchanged = |records: HashMap<BlockHash, Vec<u8>>| -> BTreeMap<String, Vec<u8>> {
            let labelled = records.into_iter().map(|(h, b)| (self.labels[&h].clone(), b));
            labelled.filter(|(l, _)| !changes.contains_key(l)).collect()
        };
        assert_eq!(unchanged(read), unchanged(written), "{at}: the reloaded records");
    }

    /// The block tree as a flush leaves it: every entry's record. The dirty set is left empty.
    fn records(&mut self) -> HashMap<BlockHash, Vec<u8>> {
        let ids: Vec<BlockId> = self.chain.ids().collect();
        for id in ids {
            self.chain.mark_dirty(id);
        }
        self.chain.take_dirty().into_iter().map(|w| (w.hash, w.bytes)).collect()
    }

    /// Every entry, by label: its `nStatus`, height, parent and chain work.
    fn index(&self) -> BTreeMap<String, (u32, u32, Option<String>, Work)> {
        let chain = &self.chain;
        let entry = |id| chain.shared.arena.imm(id, |e| (e.height, e.prev)).expect("resident");
        chain
            .ids()
            .map(|id| {
                let (height, prev) = entry(id);
                let status = chain.writer_status(id).expect("known").bits();
                (self.label(id), (status, height, prev.map(|p| self.label(p)), chain.work(id)))
            })
            .collect()
    }

    /// A node chain loaded from `records`, the replay's services kept.
    fn reload_with(
        &mut self,
        records: impl Iterator<Item = (BlockHash, Vec<u8>)>,
        checkpoints: bool,
    ) {
        let cfg = ChainConfig {
            checkpoints_enabled: checkpoints,
            ..ChainConfig::node(&self.params)
        };
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().expect("a pool");
        let svc = self.chain.svc.clone();
        self.chain = HeaderChain::load(self.params.clone(), cfg, svc, records.map(Ok), &pool)
            .expect("reloads");
    }

    /// Sets the chain's state to `view`'s, for an op the chain does not replay.
    fn inject(&mut self, view: &Value) {
        self.set_tip(self.id(label(&view["tip"])));
        let tied = best_header_set(&view["best_header"]);
        let ours = self.label(self.chain.best_header());
        // A tie keeps the chain's own choice, which must be one of the tied entries (D9).
        let best = match tied.len() {
            1 => self.id(tied.first().expect("one")),
            _ => {
                assert!(tied.contains(&ours), "best header {ours}, not one of {tied:?}");
                self.chain.best_header()
            }
        };
        self.chain.best_header = Some(self.chain.facts_of(best).expect("known"));
        for (l, status) in status_changes(view) {
            let id = self.id(&l);
            self.chain.set_status(id, |_| Status(status)).expect("known id");
        }
        let failed: Vec<BlockId> =
            labels_of(&view["failed_blocks"]).iter().map(|l| self.id(l)).collect();
        self.chain.failed.clear();
        for id in failed {
            self.chain.failed.insert(id);
        }
        let cl = &view["cl"];
        self.chain.cl_best = cl["best"].as_str().map(|best| self.lock_ref(best));
        self.chain.cl_floor = cl["floor"].as_str().map(|l| self.chain.block_ref(self.id(l)));
        self.sporks.set(cl["spork_active"].as_bool().expect("spork_active"));
    }

    /// A lock as the harness prints it, `L@h`. A lock on a block the harness never built (`?X`)
    /// names no indexed hash.
    fn lock_ref(&self, lock: &str) -> ChainLockRef {
        let (l, height) = lock.split_once('@').expect("label@height");
        ChainLockRef {
            height: height.parse().expect("a height"),
            hash: match l.strip_prefix('?') {
                Some(_) => BlockHash::from([0xff; 32]),
                None => self.hash(l),
            },
        }
    }

    /// Moves the active tip to `to`: disconnects to the fork, then connects up (after a reload,
    /// Core's `LoadChainTip`).
    fn set_tip(&mut self, to: BlockId) {
        if self.chain.private_tip().is_none() {
            self.chain.init_active_tip(to).expect("Core LoadChainTip");
            return;
        }
        let fork = self.chain.find_fork_private(to).expect("a tip");
        while self.chain.private_tip().expect("a tip").0.id != fork {
            self.chain.disconnect_tip().expect("above genesis");
        }
        let arena = &self.chain.shared.arena;
        let mut path: Vec<BlockId> =
            std::iter::successors(Some(to), |&id| arena.imm(id, |e| e.prev).flatten())
                .take_while(|&id| id != fork)
                .collect();
        path.reverse();
        for id in path {
            self.chain.connect_tip(id).expect("prev is the tip");
        }
    }
}

/// Core's `ResultName` (`harness.cpp`).
fn class_name(r: R) -> &'static str {
    match r {
        R::Unset => "unset",
        R::Consensus => "consensus",
        R::RecentConsensusChange => "recent-consensus-change",
        R::CachedInvalid => "cached-invalid",
        R::InvalidHeader => "invalid-header",
        R::Mutated => "mutated",
        R::MissingPrev => "missing-prev",
        R::InvalidPrev => "invalid-prev",
        R::TimeFuture => "time-future",
        R::Checkpoint => "checkpoint",
        R::ChainLock => "chainlock",
    }
}

/// The harness's `Expand`: labels and `A81..A90` ranges, in order.
fn expand(list: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for s in list.as_array().expect("a label list") {
        let s = s.as_str().expect("a label");
        let Some((lo, hi)) = s.split_once("..") else {
            out.push(s.to_owned());
            continue;
        };
        let digits = lo.find(|c: char| c.is_ascii_digit()).expect("a height");
        let prefix = &lo[..digits];
        let a: u32 = lo[digits..].parse().expect("height");
        let b: u32 = hi[prefix.len()..].parse().expect("height");
        out.extend((a..=b).map(|h| format!("{prefix}{h}")));
    }
    out
}

fn label(v: &Value) -> &str {
    v.as_str().unwrap_or_else(|| panic!("a label, not {v}"))
}

/// A view's best header: its label, or the tied entries of an `address_tie` (§18 D9: Core's
/// choice between them depends on memory addresses, so any one of them is Core's).
fn best_header_set(v: &Value) -> BTreeSet<String> {
    match v.get("address_tie") {
        Some(tied) => labels_of(tied),
        None => BTreeSet::from([label(v).to_owned()]),
    }
}

fn labels_of(v: &Value) -> BTreeSet<String> {
    v.as_array().expect("a label list").iter().map(|l| label(l).to_owned()).collect()
}

/// The view's `status_changes`: each label's new `nStatus`.
fn status_changes(view: &Value) -> BTreeMap<String, u32> {
    let changes = view["status_changes"].as_object().expect("status_changes");
    changes
        .iter()
        .map(|(l, s)| {
            let s = s.as_u64().unwrap_or_else(|| panic!("{l}: status {s} not replayable"));
            (l.clone(), u32::try_from(s).expect("a u32 status"))
        })
        .collect()
}

/// Core's `CDiskBlockIndex` bytes as our record: without the client-version prefix and the
/// inline hash before the header (`chain.h:391-413`).
fn core_record(core: &[u8]) -> Vec<u8> {
    let mut rest = core;
    read_varint_u32(&mut rest).expect("client version");
    let (fields, tail) = rest.split_at(rest.len() - 112);
    [fields, &tail[32..]].concat()
}

fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

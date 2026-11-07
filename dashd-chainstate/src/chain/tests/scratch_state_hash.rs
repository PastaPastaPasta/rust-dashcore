//! CH7a-5b's scratch-attempt harness (OD-CP-36, OD-CP-38): hook H11 on VerifyDB's scratch
//! target over real `NodeEvo`, for blocks the node has never committed, without production
//! VerifyDB's walk (which refuses every block past DIP0003, CH5 D2, and stays so).
//!
//! Two rigs per chain, each the node's connector over `NodeEvo` (`evo_chain`'s recorded rig),
//! each writing its state-hash records:
//! - rig A connects the chain in order; its tip `C`s are the reference;
//! - rig B, for each chosen block N in turn, stands at N−1, stores and indexes N as Core's
//!   `AcceptBlock` does without its `ActivateBestChain` (`validation.cpp:4840-4858`), and in the
//!   same V turn connects N into a `verify_scratch(ScratchParent::Tip)` view, as VerifyDB's level 4
//!   does (`validation.cpp:5165-5175`). Rig B has never committed N, so no row of N exists for a
//!   read to find but the attempt's own staging.
//!
//! The comparison is the block (`h`, `b`) and the four evo keys, never whole records: `x.seq`
//! differs on every record.

use std::cell::RefCell;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use dash_consensus::primitives::decode_block;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_statedigest::{
    Chain, FileKind, Hash256 as DHash, Header, Kind, Lines, Origin, Outcome, Record, Value, Writer,
};

use super::evo_chain::{
    Kept, MN_CHAIN, MOCKTIME, decode, linked, recorded_config_on, recorded_params,
};
use super::support::{Mined, Rig, RigConfig};
use crate::chain::seams::HeldChainLocks;
use crate::chain::{
    AttemptOrigin, BlockStatusWriter, ConnectCx, ConnectOutcome, Fatal, L3Skip, ScratchParent,
    VerifyDbArgs, VerifyDbResult, ViewTarget,
};
use crate::connect::DigestLog;
use crate::connect::verify_evo_capability;

/// O8 §5.2's evo keys.
const EVO_KEYS: [&str; 4] = ["mnl", "qc", "cp", "ehf"];

/// A state-hash log in memory, read after the sink moves into the connector.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl Write for Log {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("log").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Log {
    /// A sink writing to this log.
    fn sink(&self) -> Box<dyn crate::connect::DigestSink> {
        let header = Header {
            file: FileKind::Digest,
            src: Writer::Rdashd,
            ver: b"test".to_vec(),
            chain: Chain::Regtest,
            run: b"1-1".to_vec(),
        };
        Box::new(DigestLog::new(self.clone(), header).expect("the H is written"))
    }

    /// The `C` records of `org`.
    fn connects(&self, org: Origin) -> Vec<Outcome> {
        let bytes = self.0.lock().expect("log").clone();
        Lines::new(&bytes)
            .filter_map(|(_, r)| match r.expect("a record") {
                Record::Outcome(o) if o.kind() == Kind::Connect && o.id.org == org => Some(o),
                _ => None,
            })
            .collect()
    }
}

/// What two `C`s must share (OD-CP-38 #2): the block, `h` and `b`, and the four evo keys, each
/// `None` if absent. `x.seq`, `o`, `org` and the other `s` keys are not compared.
type Compared = (i64, DHash, [Option<Value>; 4]);

fn compared(c: &Outcome) -> Compared {
    (c.id.h, c.id.b, EVO_KEYS.map(|k| c.s().get(k).cloned()))
}

/// A recorded chain: its parameters, mocktime and blocks after genesis (element `i` at height
/// `i + 1`).
struct Recorded {
    p: Arc<ChainSetup>,
    mocktime: i64,
    blocks: Vec<Mined>,
}

/// dashd's masternode chain (`tests/data/ch7a5`): DIP0003 at 48, to 300.
fn mn_chain() -> Recorded {
    let p = recorded_params();
    let blocks = linked(&Mined::genesis(&p), decode(MN_CHAIN));
    Recorded {
        p,
        mocktime: MOCKTIME,
        blocks,
    }
}

/// The O8 recorder's `feature_asset_locks` node0 (`test-vectors/statedigest/regtest-v24`): its
/// `chain_args` as `ChainParamsBuilder` options, asserted equal to `extract.json`'s, so an
/// argument this mapping lacks fails here; its blocks from `blocks.dat` (blk records: magic,
/// size, block), genesis first.
fn asset_locks() -> Recorded {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../test-vectors/statedigest/regtest-v24/feature_asset_locks/node0");
    let extract: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(dir.join("extract.json")).expect("extract"))
            .expect("extract.json");
    let args: Vec<&str> = extract["chain_args"]
        .as_array()
        .expect("chain_args")
        .iter()
        .map(|a| a.as_str().expect("an argument"))
        .collect();
    assert_eq!(
        args,
        [
            "-llmqtestinstantsenddip0024=llmq_test_instantsend",
            "-dip3params=2:2",
            "-testactivationheight=v20@100",
            "-testactivationheight=mn_rr@560",
        ],
        "the mapped chain_args"
    );
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::LlmqTestInstantSendDip0024, "llmq_test_instantsend")
        .arg(ChainOption::Dip3Params, "2:2")
        .arg(ChainOption::TestActivationHeight, "v20@100")
        .arg(ChainOption::TestActivationHeight, "mn_rr@560")
        .build()
        .expect("regtest");
    let mocktime = extract["mocktime"].as_i64().expect("mocktime");
    let raw = zstd::decode_all(std::fs::File::open(dir.join("blocks.dat.zst")).expect("blocks"))
        .expect("zstd");
    let (mut rest, mut blocks) = (raw.as_slice(), Vec::new());
    while let Some((record, tail)) = rest.split_at_checked(8) {
        assert_eq!(record[..4], p.message_start, "the network's magic");
        let size = u32::from_le_bytes(record[4..].try_into().expect("a size"));
        let (mut bytes, tail) = tail.split_at(usize::try_from(size).expect("a size"));
        blocks.push(Arc::new(decode_block(&mut bytes).expect("a block").into_value()));
        rest = tail;
    }
    let genesis = Mined::genesis(&p);
    assert_eq!(blocks[0].hash(), genesis.hash(), "genesis first");
    let blocks = linked(&genesis, blocks.split_off(1));
    Recorded {
        p: Arc::new(p),
        mocktime,
        blocks,
    }
}

impl Recorded {
    /// Block `h` (from 1).
    fn at(&self, h: u32) -> &Mined {
        &self.blocks[h as usize - 1]
    }

    /// A rig of this chain writing to `log`, and its evo layer and coins' tips.
    fn rig(&self, log: &Log) -> (Rig, Kept) {
        let cfg = RigConfig {
            params: Some(self.p.clone()),
            ..RigConfig::default()
        };
        let (cfg, kept) = recorded_config_on(cfg, self.p.clone(), self.mocktime, Some(log.sink()));
        (Rig::with(cfg), kept)
    }
}

/// Rig A (OD-CP-38 #1): the whole chain connected in order, and its tip `C`s.
fn reference(chain: &Recorded) -> (Rig, Kept, Vec<Outcome>) {
    let log = Log::default();
    let (rig, kept) = chain.rig(&log);
    for m in &chain.blocks {
        rig.accept(m);
    }
    kept.assert_no_abort(&rig);
    let tip = log.connects(Origin::Tip);
    assert_eq!(tip.len(), chain.blocks.len() + 1, "a tip C per block, genesis's included");
    (rig, kept, tip)
}

/// Rig B (OD-CP-38 #1): for each of `heights`, ascending, the rig stands at N−1, then in one V
/// turn stores and indexes N without activating it and connects it into a fresh scratch view.
/// Each scratch connect must be `Connected` and leave V's tip, the active coins and the evo
/// layer at N−1. The scratch attempts' `C`s, in order.
fn scratch_reconnects(chain: &Recorded, heights: &[u32]) -> Vec<Outcome> {
    let log = Log::default();
    let (rig, kept) = chain.rig(&log);
    let mut next = 1;
    for &n in heights {
        assert!(n >= next, "ascending heights");
        for h in next..n {
            rig.accept(chain.at(h));
        }
        let parent = chain.at(n - 1);
        let m = chain.at(n).clone();
        rig.headers(std::slice::from_ref(&m));
        let id = rig.id(&m);
        let out = Arc::new(Mutex::new(None));
        let put = out.clone();
        rig.on_v(move |v| {
            // Core `AcceptBlock`'s write and `ReceivedBlockTransactions` (`v:4840-4858`), with
            // no `ActivateBestChain` after them: N is stored and indexed, never connected.
            let pos = v.files.save_block(&m.block, m.height, None).map_err(Fatal::new)?;
            let pos = pos.ok_or_else(|| Fatal::new("no position for the block"))?;
            let n_tx = u32::try_from(m.block.txs().len()).expect("a count");
            v.idx.received_block_transactions(&mut v.chain, &v.reader, id, n_tx, pos)?;
            // VerifyDB's scratch attempt (`verify.rs` `scratch_attempt`) on V's own context.
            let mut view = v
                .connector
                .verify_scratch(ScratchParent::Tip)?
                .ok_or_else(|| Fatal::new("a connector without a scratch view"))?;
            let deployments = v.deployments.view();
            let is_ibd = v.is_ibd();
            let best = v.cl.sigs.best.clone();
            let chain = RefCell::new(&mut v.chain);
            let best_header = chain.borrow().best_header();
            let chainlocks = HeldChainLocks::new(&chain, best);
            let cx = ConnectCx {
                params: &v.params,
                reader: &v.reader,
                deployments: &deployments,
                best_header,
                min_chain_work: v.reader.min_chain_work(),
                is_ibd,
                chainlocks: &chainlocks,
                guard: v.guard,
                origin: AttemptOrigin::VerifyDb,
            };
            let mut st = BlockStatusWriter::new(&chain, id);
            let target = ViewTarget::Scratch(&mut *view);
            let r =
                v.connector.connect_block(&cx, &mut v.vcx, target, &m.block, false, id, &mut st);
            *put.lock().expect("out") = Some(r);
            Ok(())
        });
        let r = out.lock().expect("out").take().expect("V ran it");
        assert_eq!(r, Ok(ConnectOutcome::Connected), "the scratch connect of {n}");
        assert_eq!(rig.v.tip_watch().get(), Some(parent.hash()), "V's tip after {n}'s");
        assert_eq!(kept.evo_tip(), Some(parent.hash()), "the evo layer after {n}'s");
        assert_eq!(kept.coins_best(), Some(parent.hash()), "the active coins after {n}'s");
        next = n;
    }
    kept.assert_no_abort(&rig);
    log.connects(Origin::Verifydb)
}

/// Each scratch `C` against rig A's tip `C` of its block: one per cycle, in order, the block
/// and the four evo keys equal.
fn assert_scratch_matches(tip: &[Outcome], scratch: &[Outcome], heights: &[u32]) {
    let got: Vec<i64> = scratch.iter().map(|c| c.id.h).collect();
    let want: Vec<i64> = heights.iter().map(|&h| h.into()).collect();
    assert!(!want.is_empty());
    assert_eq!(got, want, "one verifydb C per cycle");
    for c in scratch {
        let reference = &tip[usize::try_from(c.id.h).expect("a height")];
        assert_eq!(compared(c), compared(reference), "at {}", c.id.h);
    }
}

/// A `cp` value's fields: `locked`, `currentLimit`, `latelyUnlocked`.
fn pool(c: &Outcome) -> Option<[i64; 3]> {
    let cp = c.s().get("cp")?.as_arr()?;
    Some([cp[0].as_int()?, cp[1].as_int()?, cp[2].as_int()?])
}

/// OD-CP-36/38: a scratch reconnect of a block the node never committed hashes the list, pool
/// and signals that attempt staged, as the tip connect did. On dashd's masternode chain at the
/// EvoNode's registration (116), its ProUpServTx (141) and the collateral spend (201); on
/// `feature_asset_locks` at V20's first block (100), its first lock and first unlock, the first
/// MNHF signal (188), the tip (978), with `cp` and `ehf` in era, and two blocks that mine
/// commitments (179 and 779, three and four `qc` rows in dashd's golden).
#[test]
fn scratch_reconnects_carry_the_tips_evo_state_hashes() {
    let mn = mn_chain();
    let (_rig, _kept, tip) = reference(&mn);
    let heights = [116, 141, 201];
    assert_scratch_matches(&tip, &scratch_reconnects(&mn, &heights), &heights);

    let locks = asset_locks();
    let (_rig, _kept, tip) = reference(&locks);
    assert_eq!(tip.len(), 979);
    let first = |f: &dyn Fn([i64; 3], [i64; 3]) -> bool| -> u32 {
        (101..tip.len())
            .find(|&h| {
                let (Some(was), Some(is)) = (pool(&tip[h - 1]), pool(&tip[h])) else {
                    return false;
                };
                f(was, is)
            })
            .and_then(|h| u32::try_from(h).ok())
            .expect("the event exists")
    };
    let lock = first(&|was, is| is[0] > was[0]);
    let unlock = first(&|was, is| is[2] > was[2]);
    let mut heights = vec![100, lock, unlock, 179, 188, 779, 978];
    heights.sort_unstable();
    heights.dedup();
    let scratch = scratch_reconnects(&locks, &heights);
    assert_scratch_matches(&tip, &scratch, &heights);
    for c in &scratch {
        assert!(c.s().get("cp").is_some_and(|v| *v != Value::Null), "cp at {}", c.id.h);
        let ehf = c.s().get("ehf").and_then(Value::as_arr).expect("ehf in era");
        if c.id.h >= 188 {
            assert!(!ehf.is_empty(), "ehf at {}", c.id.h);
        }
    }
    let rows = |h: i64| {
        let c = scratch.iter().find(|c| c.id.h == h).expect("a scratch C");
        c.s().get("qc").and_then(Value::as_arr).map(<[Value]>::len)
    };
    assert_eq!((rows(179), rows(779)), (Some(3), Some(4)), "the scratch qc rows");
}

/// CH5 D2 unchanged: production VerifyDB still skips level 3 at the first block past DIP0003
/// with complete hooks, so level 4 does not run, on the asset-lock chain at its tip.
#[test]
fn production_verifydb_still_refuses_past_dip3() {
    let (rig, _kept, _tip) = reference(&asset_locks());
    let r = rig
        .v
        .verify_db(VerifyDbArgs {
            level: 4,
            depth: 6,
        })
        .recv()
        .expect("V answers");
    let capability = verify_evo_capability("DIP0003");
    assert!(
        matches!(&r, VerifyDbResult::SkippedL3Checks(L3Skip::NotReady(text)) if text.contains(&capability)),
        "{r:?}"
    );
}

//! BL-648 T2: dashd v24.0.0's recorded V19-window chains
//! (`dashd-chainstate/tests/data/v19-window`, PROVENANCE.md) given to a fresh dashd and to V side
//! by side, on the recordings' chain arguments and a clock a minute after their last block.
//!
//! - R1 (V19 at 212, ChainLocks off): M to 215, `invalidateblock 209`, S (209-214), a restart of
//!   both on S's 214, `reconsiderblock 209`, M to 259. After each block both agree on the tip,
//!   `getdeploymentinfo` and `protx list registered true`, field by field (operator key and state
//!   version included). N1: at S's 210, a block at 211 (B, the last block under legacy) holding
//!   M's ProRegTx v2 from 212 is `bad-protx-version` on both.
//! - R2 (V20 and MN_RR at 100, V19 at 236, ChainLocks on): every block, and each recorded
//!   ChainLock at its arrival tip (dashd `submitchainlock`, V `ClIntake` and `RecordChainLock`),
//!   both agreeing at each block. At 235, with the ChainLock on 234 the best: both templates' CbTx
//!   carries a null ChainLock (`node/miner.cpp:117-124`), and `switch-refused.txt` gets the same
//!   reject reason from both, `bad-cbtx-cldiff` (its 234 signature reads as no basic point, so a
//!   null signature at difference 1). Then the chain alone, no ChainLock: the same.
//! - R3 (V19 at 150, BL-839): every block, both agreeing at each, with a ProUpServTx v1 at 150
//!   and a ProUpRevTx v1 at 151 signed under the basic scheme (Core verifies a ProTx's operator
//!   signature under the block's scheme, not its payload version). `serv-refused.txt` at 149 and
//!   `rev-refused.txt` at 150, the same payloads signed under legacy, are `bad-protx-sig` on both.
//!
//! Needs dashd v24.0.0 (see `v24`).

mod v24;

use std::sync::{Arc, Mutex};

use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::{Header, core_serialize};
use dash_consensus::{Block, Tx};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{BlockState, ValidationInterface};
use dashd_mining::{BlockTemplateSource, ChainTemplateSource, Grind, TemplateTxs, grind};
use dashd_rpc::key_io::KeyIoParams;
use serde_json::{Value, json};

use v24::mnlist::{dashd_list, key_io, ours};
use v24::{Harness, blocks, hex, unhex};

const R1_CHAIN: &str = include_str!("../../dashd-chainstate/tests/data/v19-window/r1/chain.txt");
const R1_FORK: &str = include_str!("../../dashd-chainstate/tests/data/v19-window/r1/fork.txt");
const R1_SUMMARY: &str =
    include_str!("../../dashd-chainstate/tests/data/v19-window/r1/summary.json");
const R2_CHAIN: &str = include_str!("../../dashd-chainstate/tests/data/v19-window/r2/chain.txt");
const R2_CLSIGS: &str =
    include_str!("../../dashd-chainstate/tests/data/v19-window/r2/clsigs.jsonl");
const R2_REFUSED: &str =
    include_str!("../../dashd-chainstate/tests/data/v19-window/r2/switch-refused.txt");
const R2_SUMMARY: &str =
    include_str!("../../dashd-chainstate/tests/data/v19-window/r2/summary.json");
const R3_CHAIN: &str = include_str!("../../dashd-chainstate/tests/data/v19-window/r3/chain.txt");
const R3_SERV_REFUSED: &str =
    include_str!("../../dashd-chainstate/tests/data/v19-window/r3/serv-refused.txt");
const R3_REV_REFUSED: &str =
    include_str!("../../dashd-chainstate/tests/data/v19-window/r3/rev-refused.txt");
const R3_SUMMARY: &str =
    include_str!("../../dashd-chainstate/tests/data/v19-window/r3/summary.json");

/// Both recordings' DIP0003 block (`-dip3params=2:2`).
const DIP3: usize = 2;
/// R1's fork point P (S starts at P + 1), and M's tip when S was mined.
const R1_P: usize = 208;
const R1_M_AT_FORK: usize = 215;
/// R2's V19 height.
const N2: usize = 236;
/// R3's V19 height.
const N3: usize = 150;
/// A template's coinbase script.
const SCRIPT: [u8; 1] = [0x51];

/// `submitblock_StateCatcher` on V's signals: the state of each block checked, by hash.
#[derive(Default)]
struct Checked(Mutex<Vec<(BlockHash, BlockState)>>);

impl ValidationInterface for Checked {
    fn block_checked(&self, block: &Block, state: &BlockState, _ibd: bool) {
        self.0.lock().unwrap().push((block.hash(), state.clone()));
    }
}

/// `submitblock`'s answer for a checked block (`BIP22ValidationResult`): null when valid, else
/// the reject reason.
fn bip22(state: &BlockState) -> Value {
    match state {
        BlockState::Valid => Value::Null,
        BlockState::Invalid(e) => json!(e.reject_reason()),
        BlockState::Error(msg) => json!(msg),
    }
}

/// Both nodes on a recording's chain arguments (`summary`'s), with a clock a minute after its
/// last block; `None` when the dashd tests are skipped.
fn start(summary: &Value, chain: &[Arc<Block>]) -> Option<Harness> {
    let args = summary["chain_args"].as_array().expect("chain_args");
    let args: Vec<&str> = args.iter().map(|a| a.as_str().expect("an arg")).collect();
    let mocktime = i64::from(chain.last().expect("blocks").header().time()) + 60;
    Harness::start_at(&args, mocktime)
}

/// Both nodes agree at `height` on `tip`, the deployments, and dashd's `protx list` at the height
/// against V's list at its tip.
fn agree(h: &Harness, tip: &BlockHash, height: usize, key_io: &KeyIoParams) {
    let (list, _) = h.assert_agree_at(tip, height, DIP3);
    assert_eq!(ours(&list, key_io), dashd_list(h, height), "protx list at {height}");
}

/// `block` to both, the answers compared: dashd's `submitblock`, and the state V's
/// `BlockChecked` carries. dashd's answer.
fn both_check(h: &Harness, checked: &Checked, block: &Arc<Block>) -> Value {
    let core = h.dashd().call("submitblock", json!([hex(&core_serialize(&**block))]));
    let chain = h.sut.chain().expect("V runs");
    chain.process_block(Arc::clone(block), true, None, true).recv().expect("V");
    assert!(h.sut.sync_signals(), "V's signal queue drained");
    let seen = checked.0.lock().unwrap();
    let state = seen.iter().rev().find(|(hash, _)| *hash == block.hash()).expect("V checked it");
    assert_eq!(bip22(&state.1), core, "{}", block.hash());
    core
}

/// `invalidateblock` or `reconsiderblock` of `hash` on both.
fn both(h: &Harness, method: &str, hash: BlockHash) {
    h.dashd().call(method, json!([hash.to_string()]));
    let chain = h.sut.chain().expect("V runs");
    let r = match method {
        "invalidateblock" => chain.invalidate_block(hash).recv(),
        _ => chain.reconsider_block(hash, false).recv(),
    };
    assert_eq!(r, Ok(Ok(())), "{method}");
}

/// N1: S's 211 with M's ProRegTx v2 (from 212) added, ground again.
fn n1(s211: &Block, m212: &Block, txid: &str, pow: &dash_consensus::pow::PowParams) -> Block {
    let reg = m212.txs().iter().find(|t| t.txid().to_string() == txid).expect("the ProRegTx");
    let mut txs: Vec<Arc<Tx>> = s211.txs().to_vec();
    txs.push(Arc::clone(reg));
    let mut block = Block::new(Header::new(*s211.header().raw()), txs);
    let mut tries = 1_000_000;
    assert_eq!(grind(&mut block, pow, &mut tries, &|| false), Grind::Found);
    block
}

fn r1() {
    let (m, s) = (blocks(R1_CHAIN), blocks(R1_FORK));
    assert_eq!((m.len(), s.len()), (259, 6));
    let summary: Value = serde_json::from_str(R1_SUMMARY).expect("summary.json");
    let Some(mut h) = start(&summary, &m) else {
        return;
    };
    let reg = &summary["events"]["reg_e"];
    assert_eq!((reg["height"].as_u64(), reg["version"].as_u64()), (Some(212), Some(2)));
    let key_io = key_io(&h.sut);
    let checked = Arc::new(Checked::default());
    assert!(h.sut.register_listener(checked.clone()));
    let pow = *HeaderParams::from_chain_params(h.sut.params()).unwrap().pow();

    for (i, b) in m[..R1_M_AT_FORK].iter().enumerate() {
        h.submit(b, i + 1);
        agree(&h, &b.hash(), i + 1, &key_io);
    }
    let cut = m[R1_P].hash();
    both(&h, "invalidateblock", cut);
    agree(&h, &m[R1_P - 1].hash(), R1_P, &key_io);
    for (i, b) in s.iter().enumerate() {
        let height = R1_P + 1 + i;
        if height == 211 {
            let block = Arc::new(n1(b, &m[211], reg["txid"].as_str().unwrap(), &pow));
            assert_eq!(both_check(&h, &checked, &block), json!("bad-protx-version"), "N1");
            agree(&h, &s[1].hash(), 210, &key_io);
        }
        h.submit(b, height);
        agree(&h, &b.hash(), height, &key_io);
    }
    h.restart();
    agree(&h, &s[5].hash(), 214, &key_io);
    both(&h, "reconsiderblock", cut);
    agree(&h, &m[R1_M_AT_FORK - 1].hash(), R1_M_AT_FORK, &key_io);
    for (i, b) in m.iter().enumerate().skip(R1_M_AT_FORK) {
        h.submit(b, i + 1);
        agree(&h, &b.hash(), i + 1, &key_io);
    }
}

/// One of R2's recorded ChainLocks: height, signature (wire bytes), arrival tip.
fn locks(chain: &[Arc<Block>]) -> Vec<(usize, Vec<u8>, usize)> {
    let mut locks: Vec<_> = R2_CLSIGS
        .lines()
        .map(|line| {
            let l: Value = serde_json::from_str(line).expect("a lock");
            let wire = unhex(l["wire"].as_str().expect("wire"));
            let height = u32::from_le_bytes(wire[..4].try_into().unwrap()) as usize;
            assert_eq!(chain[height - 1].hash().as_bytes()[..], wire[4..36], "{height}");
            (height, wire[36..].to_vec(), l["arrival_tip"].as_u64().expect("arrival") as usize)
        })
        .collect();
    locks.sort_by_key(|&(height, _, arrival)| (arrival, height));
    locks
}

/// dashd's best ChainLock height and V's, -1 for none.
fn best(h: &Harness) -> [i64; 2] {
    let core = h.dashd().try_call("getbestchainlock", json!([]));
    let core = core.map_or(-1, |cl| cl["height"].as_i64().expect("height"));
    let chain = h.sut.chain().expect("V runs");
    [core, i64::from(chain.query(|q| q.cl.best_height()).recv().expect("V serves"))]
}

/// The CbTx best-ChainLock fields of `payload`: difference (one byte below 253) and signature.
fn cl_fields(payload: &[u8]) -> (u8, Vec<u8>) {
    (payload[70], payload[71..167].to_vec())
}

/// R2 to both; with `fed`, each recorded ChainLock at its arrival tip. At 235 (before the locks
/// first seen there) the templates and `switch-refused.txt`.
fn r2(fed: bool) {
    let chain = blocks(R2_CHAIN);
    assert_eq!(chain.len(), 290);
    let [refused] = <[Arc<Block>; 1]>::try_from(blocks(R2_REFUSED)).ok().expect("one block");
    assert_eq!(refused.header().prev_hash(), chain[N2 - 2].hash());
    let summary: Value = serde_json::from_str(R2_SUMMARY).expect("summary.json");
    let Some(mut h) = start(&summary, &chain) else {
        return;
    };
    let key_io = key_io(&h.sut);
    let checked = Arc::new(Checked::default());
    assert!(h.sut.register_listener(checked.clone()));
    let locks = if fed {
        locks(&chain)
    } else {
        Vec::new()
    };
    let mut fed_locks = locks.iter().peekable();
    for (i, b) in chain.iter().enumerate() {
        let height = i + 1;
        h.submit(b, height);
        agree(&h, &b.hash(), height, &key_io);
        if height == N2 - 1 {
            let below_v19 = if fed {
                N2 as i64 - 2
            } else {
                -1
            };
            assert_eq!(best(&h), [below_v19; 2], "the best ChainLocks at {height}");

            let gbt = h.dashd().call("getblocktemplate", json!([]));
            assert_eq!(gbt["height"], json!(N2));
            let core = unhex(gbt["coinbase_payload"].as_str().expect("a CbTx"));
            let clock: Arc<dyn dash_clock::Clock> = h.sut.clock().clone();
            let chain = h.sut.chain().expect("V runs").clone();
            let source =
                ChainTemplateSource::new(chain, Arc::new(h.sut.params().clone()), clock, None);
            let template = source.create_new_block(&SCRIPT, TemplateTxs::None).expect("a template");
            assert_eq!(template.height as usize, N2);
            assert_eq!(
                hex(template.block.txs()[0].extra_payload()),
                hex(&core),
                "the CbTx at {N2}"
            );
            assert_eq!(
                cl_fields(&core),
                (0, vec![0; 96]),
                "Core's miner keeps a legacy ChainLock out"
            );

            let verdict = both_check(&h, &checked, &refused);
            assert_eq!(verdict, json!("bad-cbtx-cldiff"), "switch-refused.txt");
            agree(&h, &b.hash(), height, &key_io);
        }
        while let Some((lock, sig, _)) = fed_locks.next_if(|l| l.2 == height) {
            let hash = chain[lock - 1].hash();
            let core = h.dashd().call("submitchainlock", json!([hash.to_string(), hex(sig), lock]));
            let sig: [u8; 96] = sig[..].try_into().unwrap();
            let lock = i32::try_from(*lock).unwrap();
            h.sut.cl_record(lock, hash, sig).expect("V takes it");
            let [core_best, ours] = best(&h);
            assert_eq!(core, json!(core_best), "submitchainlock {lock}");
            assert_eq!(ours, core_best, "the best ChainLock after {lock} at {height}");
        }
    }
    assert!(fed_locks.next().is_none(), "every lock fed");
    let last = if fed {
        290
    } else {
        -1
    };
    assert_eq!(best(&h), [last; 2], "the last best ChainLocks");
}

/// R3 to both, each block agreed on; `serv-refused.txt` at 149 and `rev-refused.txt` at 150.
// Core: evo/specialtxman.cpp:1148-1155, :1482, :1617; bls/bls.cpp:323-326 at v24.0.0
fn r3() {
    let chain = blocks(R3_CHAIN);
    assert_eq!(chain.len(), 160);
    let one = |text: &str, parent: usize| -> Arc<Block> {
        let [b] = <[Arc<Block>; 1]>::try_from(blocks(text)).ok().expect("one block");
        assert_eq!(b.header().prev_hash(), chain[parent - 1].hash());
        b
    };
    let refused = [(N3, one(R3_SERV_REFUSED, N3 - 1)), (N3 + 1, one(R3_REV_REFUSED, N3))];
    let summary: Value = serde_json::from_str(R3_SUMMARY).expect("summary.json");
    assert_eq!(summary["v19_height"], json!(N3));
    let Some(h) = start(&summary, &chain) else {
        return;
    };
    let key_io = key_io(&h.sut);
    let checked = Arc::new(Checked::default());
    assert!(h.sut.register_listener(checked.clone()));
    for (i, b) in chain.iter().enumerate() {
        let height = i + 1;
        if let Some((_, block)) = refused.iter().find(|(at, _)| *at == height) {
            assert_eq!(both_check(&h, &checked, block), json!("bad-protx-sig"), "at {height}");
            agree(&h, &chain[height - 2].hash(), height - 1, &key_io);
        }
        h.submit(b, height);
        agree(&h, &b.hash(), height, &key_io);
    }
}

/// Under `sut::`, the gate's feature-step filter (`[package.metadata.gate]`).
mod sut {
    use super::*;

    #[test]
    fn r1_reorg_restart_and_n1_agree_with_dashd() {
        r1();
    }

    #[test]
    fn r2_with_its_chainlocks_agrees_with_dashd() {
        r2(true);
    }

    #[test]
    fn r2_without_chainlocks_agrees_with_dashd() {
        r2(false);
    }

    #[test]
    fn r3_v1_protxs_above_v19_agree_with_dashd() {
        r3();
    }
}

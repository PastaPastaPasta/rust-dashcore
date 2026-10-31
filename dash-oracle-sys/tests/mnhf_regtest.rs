//! `oracle_mnhf_step` (ABI 26) on the chain Core's own `feature_mnehf.py` mined: every block of
//! the active chain and of the invalidated side branch is stepped on the rows the earlier steps
//! wrote, and the proposals dashd judged with `getblocktemplate` are rechecked with
//! `just_check`.
//!
//! Provenance: `tests/data/mnhf-regtest-v24.0.json.gz`, written by `scripts/gen-mnhf-regtest.py`
//! from dashd v24.0.0-rc.3 (`evo/mnhftx.cpp` there is the pin's byte for byte; see
//! PROVENANCE.md). Two Core answers are checked:
//!
//! - `getdeploymentinfo`'s `ehf_height` at each height is `GetSignalsStage(block)` looked up at
//!   the deployment's bit (`rpc/blockchain.cpp:1492-1494`, with the stage of `:1733`), which is
//!   the step's `stage_after`. The test restarts node0 with a later testdummy start, so the bit-28
//!   signal mined at 187 has left the stage by the final arguments: that is the expiry Core's
//!   test exercises, and the re-signal at 247 is accepted, not a duplicate.
//! - The proposals' verdicts (`TestBlockValidity()`, so `fJustCheck`). Core judged the two
//!   crafted zero-signature signals `bad-mnhf-invalid` in `CheckMNHFTx()`, which
//!   `ProcessSpecialTxsInBlock()` runs before `ProcessBlock()` (`evo/specialtxman.cpp`); the step
//!   runs `ProcessBlock()` alone, which sees a second bit-28 signal in the block.

mod common;

use std::io::Read;
use std::sync::OnceLock;

use common::mnhf_vectors::{VectorBlock, context, reject};
use dash_oracle_sys::{Chain, MnhfContext, MnhfStep, Oracle, SignalChange, Verdict};
use serde_json::Value;

/// Regtest's deployment bits (`chainparams.cpp:842`, `:847`, `:856`).
const DEPLOYMENT_BITS: [(&str, u8); 3] = [("testdummy", 28), ("v24", 13), ("evo_shares", 14)];

fn fixture() -> &'static Value {
    static FIXTURE: OnceLock<Value> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/mnhf-regtest-v24.0.json.gz");
        let mut text = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(path).expect(path))
            .read_to_string(&mut text)
            .expect(path);
        serde_json::from_str(&text).expect(path)
    })
}

fn list(name: &str) -> &'static [Value] {
    fixture()[name].as_array().unwrap_or_else(|| panic!("{name}"))
}

fn chain() -> Chain {
    let args: Vec<&str> = list("chain_args").iter().map(|a| a.as_str().unwrap()).collect();
    Chain::new("regtest", &args)
}

fn header_of(block: &[u8]) -> [u8; 80] {
    block[..80].try_into().unwrap()
}

/// The version bits Core decoded in a block's MNHF transactions, in block order.
fn block_bits(block: &Value) -> Vec<u8> {
    let mut txs: Vec<(u64, u8)> = block["mnhf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["index_in_block"].as_u64().unwrap(), s["versionBit"].as_u64().unwrap() as u8))
        .collect();
    txs.sort_unstable();
    txs.into_iter().map(|t| t.1).collect()
}

/// The version bits of a block's MNHF transactions in block order, as rust-dashcore decodes
/// them (the proposals carry no decoded list; the active blocks check this against Core's).
fn decoded_bits(block: &[u8]) -> Vec<u8> {
    use dashcore::blockdata::transaction::special_transaction::TransactionPayload;
    let block: dashcore::Block = dashcore::consensus::deserialize(block).unwrap();
    block
        .txdata
        .iter()
        .skip(1)
        .filter_map(|tx| match &tx.special_transaction_payload {
            Some(TransactionPayload::MnhfSignalPayloadType(p)) => Some(p.version_bit),
            _ => None,
        })
        .collect()
}

/// The same bits in bit order, as a row lists them.
fn mined_bits(block: &Value) -> Vec<u8> {
    let mut bits = block_bits(block);
    bits.sort_unstable();
    bits
}

/// The median time past of a block on `headers` (its parent is the last).
fn block_mtp(headers: &[[u8; 80]], block: &[u8]) -> i64 {
    let mut times: Vec<u32> = headers.iter().map(|h| common::header_time(h)).collect();
    times.push(common::header_time(block));
    common::mnhf_vectors::median_time_past(&times)
}

/// A header chain and the signal rows Core stores on it: each connected block's signals, from
/// `V20Height` on (`CMNHFManager::AddToCache()`), as a change wherever a row differs from its
/// parent's.
#[derive(Clone)]
struct Walk {
    headers: Vec<[u8; 80]>,
    changes: Vec<SignalChange>,
    v20_height: i32,
}

impl Walk {
    fn genesis(oracle: &Oracle, chain: &Chain) -> Self {
        Self {
            headers: vec![common::hex(list("headers")[0].as_str().unwrap()).try_into().unwrap()],
            changes: vec![],
            v20_height: common::mnhf_vectors::v20_height(oracle, chain),
        }
    }

    /// The walk cut back to `height`.
    fn up_to(&self, height: usize) -> Self {
        Self {
            headers: self.headers[..=height].to_vec(),
            changes: self.changes.iter().filter(|c| c.height as usize <= height).cloned().collect(),
            v20_height: self.v20_height,
        }
    }

    fn mnhf(&self, just_check: bool) -> MnhfContext {
        MnhfContext {
            just_check,
            signals: self.changes.clone(),
            ..MnhfContext::default()
        }
    }

    /// Connects Core's block `b` on the tip, checks what Core holds of every connected block
    /// and that its new signals are the ones Core decoded, and records its row. Returns the
    /// block's median time past and the step.
    fn push(&mut self, oracle: &Oracle, chain: &Chain, b: &Value) -> (i64, MnhfStep) {
        let height = self.headers.len() as i32;
        assert_eq!(b["height"], height);
        let block = common::hex(b["hex"].as_str().unwrap());
        assert_eq!(decoded_bits(&block), block_bits(b), "block {height}");
        let step = oracle
            .mnhf_step(chain, &block, &context(&self.headers, 0), &self.mnhf(false))
            .unwrap_or_else(|e| panic!("block {height}: {e:?}"));
        assert_eq!(step.verdict, Verdict::Accept, "block {height}");
        let connected = step.connected.as_ref().unwrap_or_else(|| panic!("block {height}"));
        assert!(connected.undo_ok, "block {height}");
        assert_eq!(connected.stage_undone, step.stage_before, "block {height}");
        let row = step.signals.clone().unwrap();
        let new: Vec<u8> = row.iter().filter(|s| s.1 == height).map(|s| s.0).collect();
        assert_eq!(new, mined_bits(b), "block {height}");

        let current = self.changes.last().map_or(&[][..], |c| &c.signals);
        if height >= self.v20_height && row != current {
            self.changes.push(SignalChange {
                height,
                signals: row,
            });
        }
        let mtp = block_mtp(&self.headers, &block);
        self.headers.push(header_of(&block));
        (mtp, step)
    }

    /// Core's proposal `p`, checked with `just_check` on its base: the base height, the block
    /// and the step.
    fn proposal(&self, oracle: &Oracle, chain: &Chain, p: &Value) -> (usize, Vec<u8>, MnhfStep) {
        let name = p["name"].as_str().unwrap();
        let base = p["base_height"].as_i64().unwrap() as usize;
        let block = common::hex(p["block_hex"].as_str().unwrap());
        let walk = self.up_to(base);
        let step = oracle
            .mnhf_step(chain, &block, &context(&walk.headers, 0), &walk.mnhf(true))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        (base, block, step)
    }
}

/// The active chain's walk and its steps, one per height from 1.
fn active_walk(oracle: &Oracle, chain: &Chain) -> (Walk, Vec<MnhfStep>) {
    let mut walk = Walk::genesis(oracle, chain);
    let steps = list("blocks").iter().map(|b| walk.push(oracle, chain, b).1).collect();
    let expected: Vec<String> =
        list("headers").iter().map(|h| h.as_str().unwrap().into()).collect();
    let got: Vec<String> = walk.headers.iter().map(|h| common::to_hex(h)).collect();
    assert_eq!(got, expected, "the blocks chain into the captured headers");
    (walk, steps)
}

/// The side branch on the active walk cut back to its fork point; calls `each` with every
/// side block, its median time past and its step.
fn side_walk(
    oracle: &Oracle,
    chain: &Chain,
    active: &Walk,
    mut each: impl FnMut(&Value, i64, &MnhfStep),
) {
    let side = list("invalidated");
    let fork = side[0]["height"].as_i64().unwrap() as usize - 1;
    let mut walk = active.up_to(fork);
    for b in side {
        let parent = oracle.header_decode(walk.headers.last().unwrap()).unwrap().hash;
        assert_eq!(common::rpc_hash(b["previousblockhash"].as_str().unwrap()), parent);
        let (mtp, step) = walk.push(oracle, chain, b);
        each(b, mtp, &step);
    }
}

#[test]
fn active_chain_stages_are_getdeploymentinfo_ehf_heights() {
    let chain = chain();
    for oracle in Oracle::all() {
        let (_, steps) = active_walk(&oracle, &chain);
        let deployments = list("deployments");
        assert_eq!(deployments.len(), steps.len() + 1);
        for (height, info) in deployments.iter().enumerate() {
            let stage = match height {
                0 => &[][..],
                h => &steps[h - 1].connected.as_ref().unwrap().stage_after[..],
            };
            if let Some(next) = steps.get(height) {
                assert_eq!(next.stage_before, stage, "height {height}");
            }
            for (name, bit) in DEPLOYMENT_BITS {
                let expected = info[name]["ehf_height"].as_i64();
                let got = stage.iter().find(|s| s.0 == bit).map(|s| i64::from(s.1));
                assert_eq!(got, expected, "{name} at height {height}");
            }
        }

        // What the walk saw, spelled out: both signals at 187 are accepted, the bit-28 one has
        // left the stage by 188 under the final testdummy start, the unknown bit 27 never
        // leaves, and 247 re-signals 28 beside v24's 13.
        let stage = |h: usize| steps[h - 1].connected.as_ref().unwrap().stage_after.clone();
        assert_eq!(stage(186), vec![]);
        assert_eq!(steps[186].signals, Some(vec![(27, 187), (28, 187)]));
        assert_eq!(stage(187), vec![(27, 187)]);
        assert_eq!(stage(246), vec![(27, 187)]);
        assert_eq!(steps[246].signals, Some(vec![(13, 247), (27, 187), (28, 247)]));
        assert_eq!(stage(256), vec![(13, 247), (27, 187), (28, 247)]);
    }
}

#[test]
fn the_invalidated_branch_connects() {
    let chain = chain();
    for oracle in Oracle::all() {
        let (walk, _) = active_walk(&oracle, &chain);
        let mut n = 0;
        side_walk(&oracle, &chain, &walk, |_, _, _| n += 1);
        assert_eq!(n, list("invalidated").len());
    }
}

#[test]
fn proposals_get_core_verdicts() {
    let chain = chain();
    for oracle in Oracle::all() {
        let (walk, steps) = active_walk(&oracle, &chain);
        let proposals = list("proposals");
        assert_eq!(proposals.len(), 7);
        for p in proposals {
            let name = p["name"].as_str().unwrap();
            let (base, _, step) = walk.proposal(&oracle, &chain, p);
            assert_eq!(step.stage_before, steps[base].stage_before, "{name}");
            assert_eq!(step.connected, None, "{name}");
            let expected = match p["core_result"].as_str() {
                None => Verdict::Accept,
                // CheckMNHFTx()'s verdict; ProcessBlock() alone sees the same bit twice.
                Some("bad-mnhf-invalid") => reject("bad-mnhf-duplicates-in-block"),
                Some(reason) => reject(reason),
            };
            assert_eq!(step.verdict, expected, "{name}");
            let accepted = step.verdict == Verdict::Accept;
            // fJustCheck returns the stage the block was checked against, without its bits.
            assert_eq!(step.signals, accepted.then(|| step.stage_before.clone()), "{name}");
        }
    }
}

/// Steps of this chain as dash-dml's L0 vectors (`dash-dml/tests/data/mnhf/regtest.txt`): the
/// signal blocks and their neighbours, the side branch's signal, and every proposal.
#[test]
fn regtest_steps_are_dash_dml_vectors() {
    let chain = chain();
    for oracle in Oracle::all() {
        let (walk, steps) = active_walk(&oracle, &chain);
        let mut lines = Vec::new();
        let blocks = list("blocks");
        for height in [1usize, 99, 100, 186, 187, 188, 246, 247, 248, 256] {
            let b = &blocks[height - 1];
            let block = common::hex(b["hex"].as_str().unwrap());
            let vector = VectorBlock {
                name: &format!("active {height}"),
                height: height as i32,
                mtp: block_mtp(&walk.headers[..height], &block),
                bits: &block_bits(b),
                just_check: false,
            };
            lines.push(common::mnhf_vectors::line(&oracle, &chain, &vector, &steps[height - 1]));
        }

        side_walk(&oracle, &chain, &walk, |b, mtp, step| {
            if !b["mnhf"].as_array().unwrap().is_empty() {
                let vector = VectorBlock {
                    name: &format!("side {}", b["height"]),
                    height: b["height"].as_i64().unwrap() as i32,
                    mtp,
                    bits: &block_bits(b),
                    just_check: false,
                };
                lines.push(common::mnhf_vectors::line(&oracle, &chain, &vector, step));
            }
        });

        for p in list("proposals") {
            let (base, block, step) = walk.proposal(&oracle, &chain, p);
            let vector = VectorBlock {
                name: &format!("proposal {}", p["name"].as_str().unwrap()),
                height: base as i32 + 1,
                mtp: block_mtp(&walk.headers[..=base], &block),
                bits: &decoded_bits(&block),
                just_check: true,
            };
            lines.push(common::mnhf_vectors::line(&oracle, &chain, &vector, &step));
        }
        common::mnhf_vectors::check_or_write(
            &oracle,
            "regtest.txt",
            "tests/mnhf_regtest.rs",
            &lines,
        );
    }
}

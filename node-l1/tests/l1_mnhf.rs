//! `diff_mnhf_step` (EV3-3): dash-dml's MNHF manager against `oracle_mnhf_step` on crafted
//! regtest chains, and the node alone on the chain Core's `feature_mnehf.py` mined
//! (`dash-oracle-sys/tests/data/mnhf-regtest-v24.0.json.gz`) against what dashd answered there.

use std::io::Read;
use std::sync::OnceLock;

use dash_consensus::ctx::{BlockId, ChainAncestry};
use dash_consensus::decode_block;
use dash_dml::mnhf::{EhfSignals, MnhfManager, MnhfView};
use dash_dml::store::{EvoDelta, MemEvoStore};
use dash_oracle_sys::Pin;
use dash_params::{
    BuriedDeployment, ChainOption, ChainParamsBuilder, ChainSetup, Network, Release,
};
use node_l1::mnhf::{Line, case_from_bytes, compare_case, seed_inputs};
use node_l1::{Outcome, target};
use serde_json::Value;

fn compared(bytes: &[u8]) -> Outcome {
    let case = case_from_bytes(bytes).expect("an input");
    compare_case(&case).unwrap_or_else(|d| panic!("{bytes:?}: diverges at {}", d.field))
}

/// Each reject reason, the check order, `fJustCheck`, expiry around a moved window start, the
/// tip's cache entry at the V20 boundary, and payloads that carry no signal.
#[test]
fn crafted_steps_agree() {
    // [tip, v20, flags, (op, a, b)*]: tip 39, V20 from 2 (v20 0) or from the block (v20 3).
    for (bytes, outcome) in [
        (vec![27, 0, 0], Outcome::BothAccept),
        (vec![27, 0, 0, 0, 13, 0, 0, 5, 1], Outcome::BothAccept),
        (vec![27, 0, 0, 0, 13, 0, 0, 13, 2], Outcome::BothReject),
        (vec![27, 0, 0, 0, 28, 0], Outcome::BothReject),
        (vec![27, 0, 0, 3, 0, 13, 0, 13, 0], Outcome::BothReject),
        (vec![27, 0, 0, 3, 0, 13, 0, 14, 3], Outcome::BothAccept),
        // A held non-EHF bit: the stage check comes first (`evo/mnhftx.cpp:232`, `:237`).
        (vec![27, 0, 0, 3, 0, 28, 0, 28, 0], Outcome::BothReject),
        (vec![27, 0, 1, 3, 0, 13, 0, 14, 0], Outcome::BothAccept),
        // The signal at 39 expires once v24 starts after its time; the re-signal is valid.
        (vec![27, 0, 0, 3, 99, 13, 5, 1, 3, 0, 13, 0], Outcome::BothAccept),
        // No signal: a trailing byte, another type.
        (vec![27, 0, 0, 1, 13, 0, 2, 13, 0, 0, 13, 0], Outcome::BothAccept),
        // The tip's entry below V20 is the first V20 block's stage (O2a-MNHF D13).
        (vec![27, 3, 0, 4, 13, 0, 0, 13, 0], Outcome::BothReject),
        (vec![27, 3, 0, 4, 13, 0, 0, 14, 0], Outcome::BothAccept),
        // A seeded bit no deployment signals still reaches the stage.
        (vec![27, 3, 0, 4, 5, 0], Outcome::BothAccept),
        (vec![27, 3, 0, 4, 5, 0, 0, 13, 0], Outcome::BothAccept),
        // Transaction versions 1, 2 and -1 carry no payload field; 4 is still special.
        (vec![27, 0, 0, 1, 13, 1, 0, 13, 0], Outcome::BothAccept),
        (vec![27, 0, 0, 1, 13, 4, 0, 13, 0], Outcome::BothAccept),
        (vec![27, 0, 0, 1, 13, 7, 0, 13, 0], Outcome::BothReject),
        (vec![27, 0, 0, 1, 13, 10, 0, 13, 0], Outcome::BothAccept),
        // `extractEHFSignal` reads a payload of any version.
        (vec![27, 0, 0, 1, 13, 2, 0, 13, 0], Outcome::BothReject),
        (vec![27, 0, 0, 1, 13, 5, 0, 13, 0], Outcome::BothReject),
        (vec![27, 0, 0, 1, 13, 8], Outcome::BothAccept),
    ] {
        assert_eq!(compared(&bytes), outcome, "{bytes:?}");
    }
    // A signal bit at VERSIONBITS_NUM_BITS is outside the domain; in transactions that carry no
    // signal (a trailing byte, version 1, another type) it is compared, beside a signal.
    assert_eq!(case_from_bytes(&[27, 0, 0, 0, 29, 0]), None);
    assert_eq!(case_from_bytes(&[27, 0, 0, 1, 29, 7]), None);
    for op in [[1, 30, 0], [1, 30, 1], [2, 30, 0]] {
        let bytes = [&[27, 0, 0][..], &op, &[0, 13, 0]].concat();
        assert_eq!(compared(&bytes), Outcome::BothAccept, "{bytes:?}");
    }
}

/// Every public seed, through the registered target: no divergence, and each outcome occurs.
#[test]
fn every_seed_agrees() {
    let t = target("diff_mnhf_step").expect("registered");
    (t.start.expect("a start"))(Pin::V24_0);
    let mut counts = [0usize; 3];
    for seed in seed_inputs() {
        match (t.compare)(&seed, Pin::V24_0) {
            Ok(Outcome::BothAccept) => counts[0] += 1,
            Ok(Outcome::BothReject) => counts[1] += 1,
            Ok(Outcome::OutOfDomain) => counts[2] += 1,
            Ok(other) => panic!("{seed:?}: {other:?}"),
            Err(d) => panic!("{seed:?}: diverges at {}", d.field),
        }
    }
    eprintln!("accept, reject, out of domain: {counts:?}");
    assert!(counts.iter().all(|&n| n > 0), "{counts:?}");
}

fn fixture() -> &'static Value {
    static FIXTURE: OnceLock<Value> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dash-oracle-sys/tests/data/mnhf-regtest-v24.0.json.gz"
        );
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

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex")).collect()
}

fn params() -> ChainSetup {
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in list("chain_args") {
        let arg = arg.as_str().expect("an argument");
        let (name, value) = arg.split_once('=').unwrap_or((arg, ""));
        builder = builder.arg(ChainOption::from_name(name).expect(name), value);
    }
    builder.build().expect("the fixture's chain")
}

/// Regtest's deployment bits (`chainparams.cpp:842`, `:847`, `:856`).
const DEPLOYMENT_BITS: [(&str, u8); 3] = [("testdummy", 28), ("v24", 13), ("evo_shares", 14)];

/// The node over Core's mined chain, as the chainstate connects it: one manager, the rows each
/// block's `process_block` gives written to the store.
struct Node {
    params: ChainSetup,
    manager: MnhfManager,
    store: MemEvoStore,
    headers: Vec<[u8; 80]>,
}

impl Node {
    fn new() -> Self {
        let genesis: [u8; 80] = unhex(list("headers")[0].as_str().unwrap()).try_into().unwrap();
        Self {
            params: params(),
            manager: MnhfManager::new(),
            store: MemEvoStore::new(),
            headers: vec![genesis],
        }
    }

    /// Connects `block` on the last header and returns the stage before and after it.
    fn connect(&mut self, block: &[u8]) -> (EhfSignals, EhfSignals) {
        let height = self.headers.len() as u32;
        self.headers.push(block[..80].try_into().unwrap());
        let line = Line::new(&self.headers);
        let decoded = decode_block(&mut &block[..]).expect("a block").into_value();
        let evo = self.store.view(&line);
        let view = MnhfView {
            manager: &self.manager,
            params: &self.params,
            chain: &line,
            store: &evo,
        };
        let id = BlockId(height);
        let before = view.signals_stage(Some(BlockId(height - 1))).expect("a stage");
        let processed = view
            .process_block(&decoded, id, false)
            .unwrap_or_else(|e| panic!("block {height}: {e}"));
        let after = view.signals_stage(Some(id)).expect("a stage");
        assert_eq!(view.undo_block(&decoded, id), Ok(true), "block {height}");
        self.store
            .apply(&EvoDelta {
                block: line.header(id).hash,
                height,
                dml: None,
                mined_commitments: Vec::new(),
                credit_data: None,
                credit_pool: None,
                mnhf_signals: processed.row,
                erased_mined: Vec::new(),
                evo_tip: None,
            })
            .expect("a row");
        (before, after)
    }

    /// `process_block` with `fJustCheck` for a proposal on the header at `base`.
    fn check(&self, base: usize, block: &[u8]) -> (EhfSignals, Result<EhfSignals, String>) {
        let mut headers = self.headers[..=base].to_vec();
        headers.push(block[..80].try_into().unwrap());
        let line = Line::new(&headers);
        let decoded = decode_block(&mut &block[..]).expect("a block").into_value();
        let evo = self.store.view(&line);
        let view = MnhfView {
            manager: &self.manager,
            params: &self.params,
            chain: &line,
            store: &evo,
        };
        let before = view.signals_stage(Some(BlockId(base as u32))).expect("a stage");
        let result = view
            .process_block(&decoded, BlockId(base as u32 + 1), true)
            .map(|processed| {
                assert_eq!(processed.row, None);
                processed.signals
            })
            .map_err(|e| e.reject_reason().expect("a verdict, not a store fault").to_owned());
        (before, result)
    }

    /// The stage after the header at `height`, read by a fresh manager from the stored rows alone,
    /// as a restarted node reads it.
    fn read_back(&self, height: u32) -> EhfSignals {
        let line = Line::new(&self.headers);
        let evo = self.store.view(&line);
        let fresh = MnhfManager::new();
        let view = MnhfView {
            manager: &fresh,
            params: &self.params,
            chain: &line,
            store: &evo,
        };
        view.signals_stage(Some(BlockId(height))).expect("a stored row")
    }
}

fn pairs(signals: EhfSignals) -> Vec<(u8, i32)> {
    signals.iter().collect()
}

fn block_hex(b: &Value) -> Vec<u8> {
    unhex(b["hex"].as_str().expect("hex"))
}

/// O2a-MNHF D9 on the node: after each block of the active chain, the stage holds each
/// deployment's `getdeploymentinfo` `ehf_height` (`rpc/blockchain.cpp:1492-1494`), including the
/// bit-28 signal leaving it under the restarted node's later testdummy start; then the invalidated
/// branch connects on its fork point, and each proposal gets Core's `fJustCheck` verdict.
#[test]
fn feature_mnehf_chain_on_the_node() {
    let mut node = Node::new();
    let deployments = list("deployments");
    let mut stages = vec![EhfSignals::new()];
    for b in list("blocks") {
        let (before, after) = node.connect(&block_hex(b));
        assert_eq!(&before, stages.last().unwrap(), "block {}", b["height"]);
        stages.push(after);
    }
    assert_eq!(deployments.len(), stages.len());
    for (height, (info, stage)) in deployments.iter().zip(&stages).enumerate() {
        for (name, bit) in DEPLOYMENT_BITS {
            let expected = info[name]["ehf_height"].as_i64();
            assert_eq!(stage.get(bit).map(i64::from), expected, "{name} at height {height}");
        }
    }
    assert_eq!(pairs(stages[187]), vec![(27, 187)]);
    assert_eq!(pairs(stages[256]), vec![(13, 247), (27, 187), (28, 247)]);
    let v20 = node.params.consensus.deployment_height(BuriedDeployment::V20).to_u32();
    for (height, stage) in stages.iter().enumerate().skip(v20 as usize) {
        assert_eq!(&node.read_back(height as u32), stage, "read back at height {height}");
    }

    for p in list("proposals") {
        let name = p["name"].as_str().unwrap();
        let base = p["base_height"].as_u64().unwrap() as usize;
        let (before, result) = node.check(base, &unhex(p["block_hex"].as_str().unwrap()));
        assert_eq!(before, stages[base], "{name}");
        let expected = match p["core_result"].as_str() {
            None => Ok(before),
            // CheckMNHFTx()'s verdict; ProcessBlock() alone sees the same bit twice.
            Some("bad-mnhf-invalid") => Err("bad-mnhf-duplicates-in-block".to_owned()),
            Some(reason) => Err(reason.to_owned()),
        };
        assert_eq!(result, expected, "{name}");
    }

    // The side branch, on the active chain cut back to its fork point (rows are kept by hash).
    let side = list("invalidated");
    let fork = side[0]["height"].as_u64().unwrap() as usize - 1;
    node.headers.truncate(fork + 1);
    let mut connected = 0;
    for b in side {
        node.connect(&block_hex(b));
        connected += 1;
    }
    assert_eq!(connected, side.len());
}

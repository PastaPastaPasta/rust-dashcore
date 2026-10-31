//! L0 harness for Core's MNHF signal processing: `tests/data/mnhf/*.txt` (O2a-MNHF; format in
//! that directory's README.md) against `dash_dml::mnhf` (EV3-3): `MnhfView::process_block`
//! (`CMNHFManager::ProcessBlock()`, `evo/mnhftx.cpp:190-262`, with
//! `CChainParams::IsValidMNActivation()`, `chainparams.cpp:109-130`), then for a connected block
//! the next block's stage (`GetSignalsStage()`, `evo/mnhftx.cpp:61-92`).
//!
//! Each line becomes a chain of `height + 1` blocks whose median times past are all the line's
//! `mtp`: a held signal passed the expiry check at its own block's time, which is at most the
//! line's, so it passes at the line's too. Where V20 is active after the block, V20 starts at its
//! parent, whose row is the line's `stage`; elsewhere V20 starts two blocks above it, and Core's
//! stage is empty.

use std::collections::BTreeMap;

use dash_consensus::ctx::{BlockId, ChainAncestry, HeaderCtx};
use dash_consensus::{Block, Header, Tx};
use dash_dml::mnhf::{EhfSignals, MnhfManager, MnhfView};
use dash_dml::store::{EvoDelta, MemEvoStore};
use dash_num::{Arith256, CompactTarget};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::Numeric as _;

/// `CMNHFManager::Signals`: version bit → the height its signal was mined at.
type Signals = BTreeMap<u8, i32>;

/// `Consensus::BIP9Deployment`'s fields the MNHF code reads.
#[derive(Clone, Copy, Debug)]
struct Deployment {
    bit: i64,
    start: i64,
    timeout: i64,
    use_ehf: bool,
}

#[derive(Debug)]
struct Case {
    name: String,
    deployments: Vec<Deployment>,
    v20_after: bool,
    height: i32,
    mtp: i64,
    stage: Signals,
    bits: Vec<u8>,
    just_check: bool,
}

/// What Core answered.
#[derive(Debug, PartialEq, Eq)]
struct Answer {
    /// `None` when accepted, else the reject reason.
    reject: Option<String>,
    signals: Option<Signals>,
    after: Option<Signals>,
}

/// The line's blocks: block `h` hashed `[h, 0xe0]`, every median time past `mtp`.
struct Line {
    tip: u32,
    mtp: i64,
}

impl ChainAncestry for Line {
    fn header(&self, id: BlockId) -> HeaderCtx {
        assert!(id.0 <= self.tip);
        let mut hash = [0u8; 32];
        hash[..4].copy_from_slice(&id.0.to_le_bytes());
        hash[31] = 0xe0;
        HeaderCtx {
            height: id.0,
            hash: BlockHash::from(hash),
            time: 0,
            bits: CompactTarget::new(0x207f_ffff),
            version: 4,
            mtp: self.mtp,
            chain_work: Arith256::ZERO,
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

fn tx(tx_type: u16, payload: Vec<u8>) -> Tx {
    Tx::new(Transaction {
        version: 3,
        tx_type: TxType::new(tx_type),
        inputs: vec![TxIn {
            prevout: OutPoint {
                hash: TxHash::from([7; 32]),
                index: 0,
            },
            script_sig: Default::default(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: 0,
            script_pubkey: vec![0x51].into(),
        }],
        lock_time: 0,
        extra_payload: payload,
    })
}

/// A coinbase stand-in, then one MNHF signal per bit (a null quorum hash and signature).
fn block(params: &ChainParams, bits: &[u8]) -> Block {
    let mut txs = vec![tx(0, vec![]).into()];
    for &bit in bits {
        let mut payload = vec![1, bit];
        payload.extend([0; 32 + 96]);
        txs.push(tx(7, payload).into());
    }
    Block::new(Header::new(params.genesis.header), txs)
}

fn params(case: &Case) -> ChainParams {
    let mut p = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .build()
        .expect("regtest params");
    assert_eq!(p.consensus.deployments.len(), case.deployments.len(), "{}", case.name);
    for (ours, core) in p.consensus.deployments.iter_mut().zip(&case.deployments) {
        ours.bit = core.bit.try_into().expect("a deployment bit");
        (ours.start_time, ours.timeout, ours.use_ehf) = (core.start, core.timeout, core.use_ehf);
    }
    let height = case.height as u32;
    p.consensus.v20_height = if case.v20_after {
        height - 1
    } else {
        height + 2
    }
    .into();
    p
}

fn to_ehf(signals: &Signals) -> EhfSignals {
    signals.iter().map(|(&b, &h)| (b, h)).collect()
}

fn from_ehf(signals: EhfSignals) -> Signals {
    signals.iter().collect()
}

/// The port's answer.
fn step(case: &Case) -> Answer {
    let params = params(case);
    let height = case.height as u32;
    let line = Line {
        tip: height,
        mtp: case.mtp,
    };
    let mut store = MemEvoStore::new();
    if case.v20_after {
        let prev = line.header(BlockId(height - 1));
        store
            .apply(&EvoDelta {
                block: prev.hash,
                height: prev.height,
                dml: None,
                mined_commitments: Vec::new(),
                credit_data: None,
                credit_pool: None,
                mnhf_signals: Some(to_ehf(&case.stage)),
                erased_mined: Vec::new(),
                evo_tip: None,
            })
            .expect("the parent's row");
    }
    let evo = store.view(&line);
    let manager = MnhfManager::new();
    let view = MnhfView {
        manager: &manager,
        params: &params,
        chain: &line,
        store: &evo,
    };
    match view.process_block(&block(&params, &case.bits), BlockId(height), case.just_check) {
        Err(e) => Answer {
            reject: Some(e.reject_reason().expect("a verdict, not a store fault").to_owned()),
            signals: None,
            after: None,
        },
        Ok(processed) => Answer {
            reject: None,
            signals: Some(from_ehf(processed.signals)),
            after: (!case.just_check)
                .then(|| from_ehf(view.signals_stage(Some(BlockId(height))).unwrap())),
        },
    }
}

fn signals(text: &str) -> Signals {
    if text == "-" {
        return Signals::new();
    }
    text.split(' ')
        .map(|p| {
            let (b, h) = p.split_once(':').unwrap();
            (b.parse().unwrap(), h.parse().unwrap())
        })
        .collect()
}

fn optional(text: &str) -> Option<Signals> {
    (text != "none").then(|| signals(text))
}

fn flag(text: &str) -> bool {
    match text {
        "0" => false,
        "1" => true,
        _ => panic!("flag {text}"),
    }
}

fn parse(line: &str) -> (Case, Answer) {
    let fields: Vec<&str> = line.split(" | ").collect();
    assert_eq!(fields.len(), 11, "{line}");
    let field = |i: usize, key: &str| {
        fields[i]
            .strip_prefix(key)
            .and_then(|v| v.strip_prefix(' '))
            .unwrap_or_else(|| panic!("field {i} is not {key}: {line}"))
    };
    let deployments = field(1, "deps")
        .split(' ')
        .map(|d| {
            let parts: Vec<&str> = d.split(':').collect();
            assert_eq!(parts.len(), 4, "{d}");
            let n = |i: usize| parts[i].parse::<i64>().unwrap();
            Deployment {
                bit: n(0),
                start: n(1),
                timeout: n(2),
                use_ehf: flag(parts[3]),
            }
        })
        .collect();
    let bits = match field(6, "bits") {
        "-" => vec![],
        b => b.split(' ').map(|v| v.parse().unwrap()).collect(),
    };
    let case = Case {
        name: fields[0].into(),
        deployments,
        v20_after: flag(field(2, "v20")),
        height: field(3, "height").parse().unwrap(),
        mtp: field(4, "mtp").parse().unwrap(),
        stage: signals(field(5, "stage")),
        bits,
        just_check: flag(field(7, "check")),
    };
    let answer = Answer {
        reject: (fields[8] != "ok").then(|| fields[8].into()),
        signals: optional(field(9, "signals")),
        after: optional(field(10, "after")),
    };
    (case, answer)
}

fn check(file: &str, min_cases: usize) {
    let path = format!("{}/tests/data/mnhf/{file}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).expect(&path);
    let mut n = 0;
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        let (case, core) = parse(line);
        assert_eq!(step(&case), core, "{}: {case:?}", case.name);
        n += 1;
    }
    assert!(n >= min_cases, "{path}: {n} cases");
}

#[test]
fn crafted_vectors() {
    check("crafted.txt", 26);
}

#[test]
fn regtest_vectors() {
    check("regtest.txt", 15);
}

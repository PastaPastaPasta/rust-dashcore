//! EV1-J J2 (the plan's J2 delta, OD-CP-73; regtest only): the L2-C rows, types 7 and 9 next to a
//! verified CbTx ChainLock on J0b's fixture, and L2-R2, the O8 recorder's stored regtest
//! recordings re-judged by dashd rc.5.
//!
//! The ChainLock fixture ([`chainlocked`]). J0b's chain ends at [`TIP`] with no CbTx ChainLock
//! (spork 19 is off on every dashd here, so none takes a best ChainLock from a CbTx,
//! `chainlock/handler.cpp:104-111`, and none signs one). Its LLMQ_TEST quorum (base 216, mined
//! 226; [`MNHF`]) is regtest's ChainLock quorum (`llmqTypeChainLocks`, `chainparams.cpp:960`) and
//! the only one of its type, so the scan at any later sign height selects it
//! (`llmq/quorumsman.cpp:754-805`). One setup block, 359, is the producer's own template with its
//! CbTx's `bestCLHeightDiff` 0 and `bestCLSignature` the CLSIG of block 358 ([`clsig`]), the merkle
//! root recomputed and the header ground; the producer takes it through `submitblock`. Its miner
//! then carries that signature into every later template with `bestCLHeightDiff` 1 ("inserting the
//! CL of the previous block", `node/miner.cpp:132-140`), so the probes at 360, built from that
//! template after it as J-D7 builds them (`getblocktemplate`, [`QuorumChain::mined_with`]), carry a
//! non-null ChainLock that each validator verifies (`evo/specialtxman.cpp:139-215`, called at
//! `:1020`).
//!
//! Every faulty ChainLock is a real one with one fault ([`ClFaults`], OD-CP-59): the same message
//! signed by a key no quorum holds, the fixture's real CLSIG of another height (357) claimed for
//! 358, or the carried CLSIG of 358 itself with `bestCLHeightDiff` 0, so claimed for 359 (the
//! height alone moved; `specialtxman.cpp:186-204`). The positive twin of the last is a fresh
//! CLSIG of 359 by the same key with `bestCLHeightDiff` 0 ([`ClFixture::newer`]): a newer
//! ChainLock than the parent's, which both validators accept. The nulled one (K-CLNULL) is Core's
//! own null signature, from the producer's template at 359. No signature is written by hand.
//!
//! What shows that V verified. dashd's side is its answer only. V's side is also its step-10
//! census ([`ClChecks`], `dashd_chainstate::evo::cl::census_under`): each probe names the
//! checks V's `AttemptClVerifier` makes on it, `(360, the claimed ChainLock height, its
//! verdict)`, one for a probe that reaches step 10 with a non-null ChainLock and none for one an
//! earlier check rejects (the special-tx loop, the credit pool, K-CLNULL's null check). So a V
//! that matches the parent's ChainLock instead of verifying it fails on the carried and the moved
//! ChainLocks, and one that allows only the parent's signature fails on the newer one.
//!
//! The fixture's limit (as J-D9 names member order for J0b): it has one LLMQ_TEST quorum, so
//! every sign height from 350 to 358 selects it, whatever the sign-height offset
//! (`SIGN_HEIGHT_OFFSET`, 8) or the selection rule. A V that signs or selects at another offset
//! or by another rule passes these rows; the request id and the message are live (a wrong one
//! breaks setup block 359 on both nodes and every accepted probe).
//!
//! L2-R2 ([`rejudge`]). Each stored recording whose node0 chain carries a type 7 or 9 tx
//! ([`RECORDINGS`]; the test finds them by decoding every recording) is replayed into a fresh
//! dashd rc.5: `submitblock` of each node0 block in turn must answer null and make it the tip,
//! as the recording's `C` records say dashd connected it. The dashd runs each segment of the
//! recording under the consensus arguments of the starts that connected it ([`segments`]) and
//! restarts on its datadir at each boundary, at the recording's own clock ([`Recording::clock`]).
//! Every block is accepted under any of a recording's argument sets, so the answers cannot show
//! the segments; the deployment states can: at each segment's last block, dashd's
//! `getdeploymentinfo` must give the golden's `s.dep` there ([`deployment_problems`]).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dash_consensus::hash::sha256d;
use dash_consensus::pow::PowParams;
use dash_consensus::primitives::{Header, core_serialize, decode_block};
use dash_consensus::{Block, Tx};
use dash_params::LlmqType;
use dash_primitives::TxType;
use dash_types::Numeric as _;
use serde_json::{Value, json};

use super::probes::{STRANGER, UNUSED_INDEX, without};
use super::{MNHF, NEWEST, QuorumChain, Signal, TIP, Unlock, assemble, sign};
use crate::v24::{self, Dashd, hex};
use crate::{CHAIN_ARGS, EHF_BIT, Probe, block_of, copy_dir, pinned, probe_block, rpc};
use dashd_mining::{Grind, grind};

// ---- the CLSIG helper (OD-CP-59) ----------------------------------------------------------

/// Core's `SignHash` (`llmq/signhash.cpp:14-22`): the hash of the LLMQ type, the quorum hash, the
/// request id and the message, in that order.
pub fn build_sign_hash(
    llmq_type: u8,
    quorum: &[u8; 32],
    id: &[u8; 32],
    msg: &[u8; 32],
) -> [u8; 32] {
    sha256d(&[&[llmq_type][..], quorum, id, msg].concat())
}

/// `GenSigRequestId` (`chainlock/clsig.cpp:248-251`): `SerializeHash(("clsig", int32 height))`.
pub fn request_id(height: u32) -> [u8; 32] {
    let height = i32::try_from(height).expect("a height");
    sha256d(&[&[5][..], b"clsig", &height.to_le_bytes()].concat())
}

/// The CLSIG of block `hash` at `height` by `key`, as quorum `quorum` (LLMQ_TEST, regtest's
/// ChainLock type) signs it: `VerifyChainLock`'s message (`chainlock/clsig.cpp:253-261`).
pub fn clsig(key: &sign::SecretKey, quorum: &[u8; 32], height: u32, hash: &[u8; 32]) -> [u8; 96] {
    let llmq_type = LlmqType::LlmqTest.to_base();
    sign::sign(key, &build_sign_hash(llmq_type, quorum, &request_id(height), hash))
}

// ---- the CbTx's ChainLock -----------------------------------------------------------------

/// A version 4 CbTx payload (`evo/cbtx.h` SERIALIZE_METHODS): version, height, two roots, then
/// `bestCLHeightDiff` (a compact size, one byte below 253), `bestCLSignature`, the credit pool
/// balance and the asset-unlock root.
const DIFF_AT: usize = 2 + 4 + 32 + 32;
const SIG_AT: usize = DIFF_AT + 1;
const PAYLOAD_LEN: usize = SIG_AT + 96 + 8 + 32;

/// The ChainLock `block`'s CbTx carries: its height diff and signature.
pub fn cl_of(block: &Block) -> (u8, [u8; 96]) {
    let payload = block.txs()[0].raw().extra_payload();
    assert_eq!((payload[..2].to_vec(), payload.len()), (vec![4, 0], PAYLOAD_LEN), "a v4 CbTx");
    (payload[DIFF_AT], payload[SIG_AT..SIG_AT + 96].try_into().unwrap())
}

/// `block` with its CbTx's ChainLock `(diff, sig)`, the merkle root recomputed and ground on `pow`.
pub fn with_cl(block: &Block, diff: u8, sig: &[u8; 96], pow: &PowParams) -> Block {
    cl_of(block);
    assert!(diff < 0xfd, "a one-byte compact size");
    let mut coinbase = block.txs()[0].to_mutable();
    {
        let mut extra_payload = coinbase.extra_payload().clone();
        extra_payload[DIFF_AT] = diff;
        coinbase = dash_primitives::Transaction::new(
            coinbase.version(),
            coinbase.tx_type(),
            coinbase.inputs().clone(),
            coinbase.outputs().clone(),
            coinbase.lock_time(),
            extra_payload,
        );
    }
    {
        let mut extra_payload = coinbase.extra_payload().clone();
        extra_payload[SIG_AT..SIG_AT + 96].copy_from_slice(sig);
        coinbase = dash_primitives::Transaction::new(
            coinbase.version(),
            coinbase.tx_type(),
            coinbase.inputs().clone(),
            coinbase.outputs().clone(),
            coinbase.lock_time(),
            extra_payload,
        );
    }
    let mut txs = block.txs().to_vec();
    txs[0] = Arc::new(Tx::new(coinbase));
    let mut out = Block::new(Header::new(*block.header().raw()), txs);
    let mut tries = 1_000_000;
    assert_eq!(grind(&mut out, pow, &mut tries, &|| false), Grind::Found);
    assert_eq!(cl_of(&out), (diff, *sig), "the ChainLock set");
    out
}

/// J0b's fixture with setup block 359 carrying the CLSIG of 358 (module docs), and the
/// ChainLocks the probes use.
pub struct ClFixture {
    /// Blocks 1 to 359; the template at 360, from the producer's `getblocktemplate`; the producer
    /// stopped at 359.
    pub chain: QuorumChain,
    /// The CLSIG of block 358, which block 359 carries with diff 0 and the template with diff 1.
    pub real: [u8; 96],
    /// Core's null signature, from the producer's template at 359.
    pub null: [u8; 96],
}

/// The fixture's faulty ChainLocks for 358, each one fault from [`ClFixture::real`].
pub struct ClFaults {
    /// The same message, signed by a key no quorum holds.
    pub other_key: [u8; 96],
    /// The real CLSIG of block 357: right key, the message of another height.
    pub other_height: [u8; 96],
}

/// Extends J0b's `chain` by the ChainLock setup block (module docs), on a copy of its stopped
/// producer in `scratch`.
pub fn chainlocked(
    mut chain: QuorumChain,
    binary: &Path,
    scratch: &Path,
    pow: &PowParams,
) -> ClFixture {
    let dir = scratch.join("cl-producer");
    copy_dir(&chain.producer, &dir);
    let p = Dashd::start_at(binary, &dir, chain.time, &CHAIN_ARGS);
    pinned(&p);
    let parent = Arc::clone(chain.setup.last().unwrap());
    assert_eq!(chain.setup.len(), TIP as usize, "J0b's setup tip");
    let empty = block_of(&rpc(&p, "generateblock", json!([chain.address, [], false]))["hex"]);
    assert_eq!(empty.header().prev_hash(), parent.hash(), "on J0b's tip");
    let (diff, null) = cl_of(&empty);
    assert_eq!(diff, 0, "no ChainLock before the setup block");
    let mnhf = &chain.quorums[MNHF];
    let real = clsig(&mnhf.key, &mnhf.hash, TIP, parent.hash().as_bytes());
    assert_ne!(real, null);
    let block = with_cl(&empty, 0, &real, pow);
    let answer = rpc(&p, "submitblock", json!([hex(&core_serialize(&block))]));
    assert_eq!(answer, Value::Null, "the producer takes setup block 359");
    let cbtx = &rpc(&p, "getblock", json!([block.hash().to_string(), 2]))["cbTx"];
    assert_eq!(cbtx["bestCLHeightDiff"], json!(0), "{cbtx}");
    assert_eq!(cbtx["bestCLSignature"], json!(hex(&real)), "{cbtx}");
    chain.setup.push(Arc::new(block));

    // The template at 360, read through getblocktemplate (J-D7), its coinbase generateblock's.
    chain.time += 150;
    rpc(&p, "setmocktime", json!([chain.time]));
    let empty = block_of(&rpc(&p, "generateblock", json!([chain.address, [], false]))["hex"]);
    let template = assemble(&empty, &rpc(&p, "getblocktemplate", json!([])), pow);
    assert_eq!(
        core_serialize(&*template.txs()[0]),
        core_serialize(&*empty.txs()[0]),
        "the assembly is generateblock's coinbase"
    );
    assert_eq!(template.txs().len(), 1, "an empty mempool");
    assert_eq!(cl_of(&template), (1, real), "the miner carries the ChainLock of 358");
    assert_eq!(template.header().prev_hash(), chain.setup.last().unwrap().hash(), "on 359");
    drop(p);
    chain.template = template;
    chain.producer = dir;
    ClFixture {
        chain,
        real,
        null,
    }
}

impl ClFixture {
    /// The hash of setup block `height`.
    fn hash_at(&self, height: u32) -> [u8; 32] {
        *self.chain.setup[height as usize - 1].hash().as_bytes()
    }

    /// The faulty ChainLocks (see [`ClFaults`]).
    pub fn faults(&self) -> ClFaults {
        let mnhf = &self.chain.quorums[MNHF];
        let other_key = clsig(&sign::key(STRANGER), &mnhf.hash, TIP, &self.hash_at(TIP));
        assert_eq!(
            clsig(&mnhf.key, &mnhf.hash, TIP, &self.hash_at(TIP)),
            self.real,
            "one fault: the key"
        );
        let other_height = clsig(&mnhf.key, &mnhf.hash, TIP - 1, &self.hash_at(TIP - 1));
        ClFaults {
            other_key,
            other_height,
        }
    }

    /// A fresh CLSIG of setup block 359 by the fixture's quorum, as [`ClFixture::real`] is of 358:
    /// a newer ChainLock than the one the parent's CbTx carries.
    pub fn newer(&self) -> [u8; 96] {
        let mnhf = &self.chain.quorums[MNHF];
        let at = TIP + 1;
        assert_eq!(self.chain.setup.len(), at as usize, "359 is the setup tip");
        let newer = clsig(&mnhf.key, &mnhf.hash, at, &self.hash_at(at));
        assert_ne!(newer, self.real);
        newer
    }
}

/// V's step-10 checks on one probe, as its `AttemptClVerifier` census records them
/// (`VerifyCensus::checks`): `(attempt height, ChainLock height, verdict)`.
pub type ClChecks = Vec<(u32, u32, Option<bool>)>;

// ---- the L2-C probes ----------------------------------------------------------------------

/// The L2-C rows' probes and controls at 360, built before any validator sees them.
pub fn probes(f: &ClFixture, binary: &Path, scratch: &Path, pow: &PowParams) -> Vec<Probe> {
    let chain = &f.chain;
    let t = &chain.template;
    let q = &chain.quorums;
    let stranger = sign::key(STRANGER);
    let p2pkh = &chain.p2pkh;
    let parent = u32::try_from(chain.setup.len()).unwrap();
    let faults = f.faults();
    let newer = f.newer();
    let carried = |b: &Block| assert_eq!(cl_of(b), (1, f.real), "the carried ChainLock");
    let nulled = |b: &Block| Arc::new(with_cl(b, 0, &f.null, pow));
    let faulty = |b: &Block, sig: &[u8; 96]| Arc::new(with_cl(b, 1, sig, pow));
    // The carried signature with diff 0: the claim moved from 358 to 359, the one fault.
    let moved = |b: &Block| Arc::new(with_cl(b, 0, &f.real, pow));
    let fresh = |b: &Block| Arc::new(with_cl(b, 0, &newer, pow));
    // V's checks: none where an earlier check rejects; one verify of the claimed height.
    let none = || Some(ClChecks::new());
    let verifies = |cl_height: u32, valid: bool| Some(vec![(parent + 1, cl_height, Some(valid))]);
    let (claimed, moved_to) = (parent - 1, parent);

    // Type 7: 7-OK's and 7-S5's signal on the template.
    let mnhf = &q[MNHF];
    let signal = Signal {
        bit: EHF_BIT,
        quorum: mnhf.hash,
    };
    let valid7 = probe_block(t, &[signal.signed(&mnhf.key)], pow);
    let forged7 = probe_block(t, &[signal.signed(&stranger)], pow);

    // Type 9: version 1, an unused index, requested at the parent's height, under the newest
    // Platform quorum; the valid ones in the producer's own blocks, as 9-OK's.
    let newest = &q[NEWEST];
    let base = Unlock {
        version: 1,
        index: UNUSED_INDEX,
        outputs: 1,
        requested: parent,
        quorum: newest.hash,
    };
    let mined = |version: u8| {
        let tx = Unlock {
            version,
            ..base
        }
        .signed(&newest.key, p2pkh);
        let block = Arc::new(chain.mined_with(binary, scratch, &tx, pow));
        (block, tx)
    };
    let (v1, tx1) = mined(1);
    let (v2, _) = mined(2);
    let forged9_in_place = probe_block(&without(&v1, &tx1), &[base.signed(&stranger, p2pkh)], pow);
    let forged9 = probe_block(t, &[base.signed(&stranger, p2pkh)], pow);
    let valid9_on_template = probe_block(t, &[base.signed(&newest.key, p2pkh)], pow);
    for b in [&valid7, &forged7, &v1, &v2, &forged9_in_place, &forged9, &valid9_on_template] {
        carried(b);
    }

    let mut out = Vec::new();
    let mut add = |row: &'static str,
                   name: &str,
                   expect: &'static str,
                   block: Arc<Block>,
                   cl_checks: Option<ClChecks>| {
        out.push(Probe {
            row,
            name: name.to_owned(),
            expect,
            block,
            below: None,
            cl_checks,
        })
    };
    let accepted = [("7-OK's signal", &valid7), ("9-OK version 1", &v1), ("9-OK version 2", &v2)];
    for (name, block) in accepted {
        let carried = format!("{name}, the carried ChainLock");
        add("L2-C1", &carried, "b", Arc::clone(block), verifies(claimed, true));
        let null = format!("{name}, the ChainLock nulled");
        add("L2-C1", &null, "b_control", nulled(block), none());
    }
    for (name, block) in [("7-OK's signal", &valid7), ("9-OK version 1", &v1)] {
        add(
            "L2-C2",
            &format!("{name}, a ChainLock by another key"),
            "b",
            faulty(block, &faults.other_key),
            verifies(claimed, false),
        );
        add(
            "L2-C2",
            &format!("{name}, the ChainLock of 357 for 358"),
            "b",
            faulty(block, &faults.other_height),
            verifies(claimed, false),
        );
        add(
            "L2-C2",
            &format!("{name}, the ChainLock of 358 for 359"),
            "b",
            moved(block),
            verifies(moved_to, false),
        );
        add(
            "L2-C2",
            &format!("{name}, the carried ChainLock"),
            "b_control",
            Arc::clone(block),
            verifies(claimed, true),
        );
        add(
            "L2-C2",
            &format!("{name}, a fresh ChainLock of 359"),
            "b_control",
            fresh(block),
            verifies(moved_to, true),
        );
    }
    add("L2-C3.7", "another key's signal", "b", Arc::clone(&forged7), none());
    let quorum7 = Arc::clone(&valid7);
    add("L2-C3.7", "the MNHF quorum's signal", "b_control", quorum7, verifies(claimed, true));
    add("L2-C3.9", "another key's unlock", "b", forged9, none());
    add("L2-C3.9", "the valid unlock in its place", "b_control", valid9_on_template, none());
    add(
        "L2-C4.7",
        "another key's signal, a ChainLock by another key",
        "b",
        faulty(&forged7, &faults.other_key),
        none(),
    );
    add(
        "L2-C4.7",
        "the MNHF quorum's signal, a ChainLock by another key",
        "b_control",
        faulty(&valid7, &faults.other_key),
        verifies(claimed, false),
    );
    add(
        "L2-C4.9",
        "another key's unlock in the producer's block, a ChainLock by another key",
        "b",
        faulty(&forged9_in_place, &faults.other_key),
        none(),
    );
    add(
        "L2-C4.9",
        "the producer's block, a ChainLock by another key",
        "b_control",
        faulty(&v1, &faults.other_key),
        verifies(claimed, false),
    );
    out
}

// ---- L2-R2: the stored recordings on dashd rc.5 -------------------------------------------

/// The O8 recorder's stored regtest v24 recordings (`contrib/node/statedigest/record-ft.py`,
/// `extract.py`; recorder v24.0.0-rc.3-8-g0a36fab32cb5).
pub const RECORDINGS_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../test-vectors/statedigest/regtest-v24");

/// A stored recording L2-R2 re-judges, pinned: its node0 blocks (genesis included), the first
/// height of each segment, and the heights of its type 7 and type 9 txs (the J2 delta's F1).
pub struct Pinned {
    pub name: &'static str,
    pub blocks: usize,
    pub segments: &'static [usize],
    pub type7: &'static [usize],
    pub type9: &'static [usize],
}

/// Every stored recording whose node0 chain carries a type 7 or 9 tx: 9 type 7 and 19 type 9.
pub const RECORDINGS: [Pinned; 7] = [
    Pinned {
        name: "feature_asset_locks",
        blocks: 979,
        segments: &[0],
        type7: &[188],
        type9: &[
            189, 212, 238, 381, 381, 381, 381, 383, 484, 645, 646, 812, 814, 816, 969, 970, 971,
            972, 976,
        ],
    },
    Pinned {
        name: "feature_evo_shares_activation",
        blocks: 242,
        segments: &[0, 209],
        type7: &[176],
        type9: &[],
    },
    Pinned {
        name: "feature_governance_cl",
        blocks: 223,
        segments: &[0],
        type7: &[181],
        type9: &[],
    },
    Pinned {
        name: "feature_llmq_chainlocks",
        blocks: 353,
        segments: &[0],
        type7: &[332],
        type9: &[],
    },
    Pinned {
        name: "feature_llmq_evo",
        blocks: 372,
        segments: &[0],
        type7: &[221],
        type9: &[],
    },
    Pinned {
        name: "feature_llmq_rotation",
        blocks: 453,
        segments: &[0],
        type7: &[301],
        type9: &[],
    },
    Pinned {
        name: "feature_mnehf",
        blocks: 257,
        segments: &[0, 177, 179, 201],
        type7: &[187, 187, 247],
        type9: &[],
    },
];

/// Regtest's network magic (`chainparams.cpp:873-876`, `pchMessageStart`), which starts each
/// record of a regtest `blocks.dat`.
const REGTEST_MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

/// A stored recording's node0: its directory and its blocks, genesis first (`blocks.dat`'s blk
/// records: the network's magic, the size, the block; `extract.py:106-112`).
pub struct Recording {
    pub name: String,
    pub dir: PathBuf,
    pub blocks: Vec<Arc<Block>>,
}

impl Recording {
    pub fn open(name: &str) -> Recording {
        let dir = Path::new(RECORDINGS_DIR).join(name).join("node0");
        let raw =
            zstd::decode_all(std::fs::File::open(dir.join("blocks.dat.zst")).unwrap()).unwrap();
        let (mut rest, mut blocks) = (raw.as_slice(), Vec::new());
        while let Some((record, tail)) = rest.split_at_checked(8) {
            assert_eq!(record[..4], REGTEST_MAGIC, "{name}: record {} is regtest's", blocks.len());
            let size = u32::from_le_bytes(record[4..].try_into().unwrap());
            let (mut bytes, tail) = tail
                .split_at_checked(usize::try_from(size).unwrap())
                .unwrap_or_else(|| panic!("{name}: record {} is cut short", blocks.len()));
            blocks.push(Arc::new(decode_block(&mut bytes).expect("a block").into_value()));
            assert!(bytes.is_empty(), "one block per record");
            rest = tail;
        }
        assert!(rest.is_empty(), "{name}: {} bytes after the last record", rest.len());
        Recording {
            name: name.to_owned(),
            dir,
            blocks,
        }
    }

    pub fn json(&self, file: &str) -> Value {
        serde_json::from_reader(std::fs::File::open(self.dir.join(file)).unwrap()).unwrap()
    }

    /// The heights of the txs of type `n`, one per tx.
    pub fn heights_of(&self, n: u16) -> Vec<usize> {
        let mut heights = Vec::new();
        for (h, block) in self.blocks.iter().enumerate() {
            let count =
                block.txs().iter().filter(|tx| tx.raw().tx_type() == TxType::new(n)).count();
            heights.extend(std::iter::repeat_n(h, count));
        }
        heights
    }

    /// The recording's golden `C` records, in order: the blocks dashd connected and its state
    /// after each.
    pub fn golden(&self) -> Vec<Value> {
        let raw = zstd::decode_all(std::fs::File::open(self.dir.join("golden.jsonl.zst")).unwrap());
        let text = String::from_utf8(raw.unwrap()).unwrap();
        text.lines()
            .map(|l| serde_json::from_str::<Value>(l).expect("a record"))
            .filter(|r| r["k"] == "C")
            .collect()
    }

    /// The blocks dashd connected, as the recording's golden has them (its `C` records): height
    /// and hash.
    pub fn connected(&self) -> Vec<(u64, String)> {
        let records = self.golden().into_iter();
        records.map(|r| (r["h"].as_u64().unwrap(), r["b"].as_str().unwrap().to_owned())).collect()
    }

    /// The clock dashd runs at: the recording's own, the last mock time the recorded node's log
    /// shows, with which the recorder started every copy it extracted from
    /// (`contrib/node/statedigest/extract.py:46-51`, `:170`, `:196`, `:236`, written at `:269`);
    /// for a run without `-mocktime` (`feature_dip3_deterministicmns`, none of [`RECORDINGS`]),
    /// T-RD's rule, a day past the latest block or `v24::T0` if later (OD-CP-44). CH7a-5b's
    /// replay runs its node at `max(T0, latest + 1 day)` for every recording; the verdicts here
    /// do not depend on which, since the only clock check on this path is `time-too-new`, which
    /// no recorded block reaches at either: at this clock by the assert below, at T0 since every
    /// recorded block time is before it.
    pub fn clock(&self) -> (i64, &'static str) {
        let latest = self.blocks.iter().map(|b| i64::from(b.header().time())).max().unwrap();
        let clock = match self.json("extract.json")["mocktime"].as_i64() {
            Some(mocktime) => (mocktime, "the recording's mocktime (extract.py:46-51, :196)"),
            None => (v24::T0.max(latest + 86_400), "max(T0, latest + 1 day) (OD-CP-44)"),
        };
        // `MAX_FUTURE_BLOCK_TIME` (`chain.h`): no block is time-too-new at this clock.
        assert!(latest <= clock.0 + 2 * 60 * 60, "{}: a block past the clock", self.name);
        clock
    }
}

/// The heights one consensus-argument set connected, `from..=to`.
#[derive(Debug)]
pub struct Segment {
    pub from: usize,
    pub to: usize,
    pub args: Vec<String>,
}

/// `arg`'s key, `-name` of `-name=value`.
fn key(arg: &str) -> &str {
    arg.split_once('=').map_or(arg, |(k, _)| k)
}

/// The arguments that set chain parameters, the ones `chainparams.cpp` reads: the recorder's
/// `CHAINPARAMS` (`contrib/node/statedigest/extract.py:41-45`), from which it derives
/// `chain_args` and refuses a run whose starts differ in any but `-vbparams` (`:115-153`).
const CHAIN_PARAM_KEYS: [&str; 18] = [
    "budgetparams",
    "dip3params",
    "fastprune",
    "highsubsidyblocks",
    "highsubsidyfactor",
    "llmqchainlocks",
    "llmqdevnetparams",
    "llmqinstantsenddip0024",
    "llmqmnhf",
    "llmqplatform",
    "llmqtestinstantsenddip0024",
    "llmqtestinstantsendparams",
    "llmqtestparams",
    "llmqtestplatformparams",
    "minimumdifficultyblocks",
    "powtargetspacing",
    "testactivationheight",
    "vbparams",
];

/// The other arguments the recorder's starts carry, none of which sets a chain parameter:
/// logging, RPC, P2P, wallet, index, mining and startup-check settings, and the recorder's own
/// (`-statedigestlog`, `-recordstatedecisions`; `-mocktime`, `-checkblocks` and `-checklevel`,
/// which it adds to a group's copy, `extract.py:170-171`).
const OTHER_KEYS: [&str; 49] = [
    "acceptnonstdtxn",
    "bind",
    "blockversion",
    "checkblocks",
    "checklevel",
    "connect",
    "createwalletbackups",
    "datadir",
    "debug",
    "debugexclude",
    "disablewallet",
    "discover",
    "dnsseed",
    "fallbackfee",
    "fixedseeds",
    "keypool",
    "listen",
    "listenonion",
    "loglevel",
    "logsourcelocations",
    "logthreadnames",
    "logtimemicros",
    "maxsigcachesize",
    "mocktime",
    "natpmp",
    "par",
    "parbls",
    "peertimeout",
    "persistmempool",
    "port",
    "printtoconsole",
    "recordstatedecisions",
    "regtest",
    "reindex",
    "rpcdoccheck",
    "rpcport",
    "rpcservertimeout",
    "rpcthreads",
    "server",
    "shrinkdebugfile",
    "spentindex",
    "sporkkey",
    "statedigestlog",
    "tinyblk",
    "uacomment",
    "unsafesqlitesync",
    "upnp",
    "v2transport",
    "whitelist",
];

/// `name`'s values as chain-parameter arguments, none for another argument; an argument neither
/// list classifies fails (closed: no argument is dropped unread).
fn chain_param(name: &str, values: &[&str], out: &mut Vec<String>) -> Result<(), String> {
    if CHAIN_PARAM_KEYS.contains(&name) {
        out.extend(values.iter().map(|v| format!("-{name}={v}")));
        Ok(())
    } else if OTHER_KEYS.contains(&name) {
        Ok(())
    } else {
        Err(format!("an unclassified argument -{name}"))
    }
}

/// A recorded start's chain-parameter arguments as `-key=value`: its command line's, then its
/// config's regtest section's, each in key order, as the recorder's `start_chainparams` lists
/// them (`extract.py:120-128`); the command line's apart. A key set in both is refused: dashd
/// would take the command line's value, and the replay must not have to choose.
fn start_chain_args(start: &Value) -> Result<(Vec<String>, Vec<String>), String> {
    let mut parts = (Vec::new(), Vec::new());
    let mut on_cmdline = BTreeSet::new();
    for (source, out) in [("cmdline", &mut parts.0), ("config", &mut parts.1)] {
        let entries = start[source].as_object().ok_or(format!("a start without its {source}"))?;
        let mut keys: Vec<&String> = entries.keys().collect();
        keys.sort();
        for k in keys {
            let name = match (source, k.strip_prefix("regtest.")) {
                ("cmdline", _) => k.as_str(),
                (_, Some(name)) => name,
                _ if k == "regtest" => continue,
                _ => return Err(format!("a config key outside regtest's section: {k}")),
            };
            let values: Option<Vec<&str>> =
                entries[k].as_array().and_then(|v| v.iter().map(Value::as_str).collect());
            let values = values.ok_or(format!("{source} {k}: not a list of strings"))?;
            let before = out.len();
            chain_param(name, &values, out)?;
            if out.len() > before {
                let both = if source == "cmdline" {
                    !on_cmdline.insert(name.to_owned())
                } else {
                    on_cmdline.contains(name)
                };
                if both {
                    return Err(format!("-{name} on both the command line and in the config"));
                }
            }
        }
    }
    Ok(parts)
}

/// `args` without their `-vbparams`.
fn but_vbparams(args: &[String]) -> Vec<String> {
    args.iter().filter(|a| key(a) != "-vbparams").cloned().collect()
}

/// The recording's segments, read from what the recorder wrote, not derived here
/// (OD-CP-73 q4); [`segments_in`] on its `extract.json` and `chainargs.json`.
pub fn segments(rec: &Recording) -> Result<Vec<Segment>, String> {
    let (extract, chainargs) = (rec.json("extract.json"), rec.json("chainargs.json"));
    segments_in(&rec.name, rec.blocks.len() - 1, &extract, &chainargs)
}

/// The segments of a recording whose tip is `tip`, from its `extract.json` and
/// `chainargs.json`.
///
/// The arguments are the starts' own. Every start's chain-parameter arguments
/// ([`start_chain_args`]) are read in full, and an argument neither [`CHAIN_PARAM_KEYS`] nor
/// [`OTHER_KEYS`] names fails. Each start's, `-vbparams` aside, must be `chain_args`', and the
/// last start's must be `chain_args` itself (`extract.py:115-118`, `:251`); all starts share
/// one config. The starts' runs (consecutive starts with equal arguments) are the segments'
/// arguments, in order.
///
/// The boundaries are the recorder's. It checks the BIP9 values of the other starts' blocks on
/// one copy per argument set and records each set's command line and the heights it checked
/// (`extract.py:159-191`, `extract.json` `bip9.groups`; `conformance` `check_bip9`: every
/// active-chain block with a tip `C` from those starts), and every other height is the last
/// start's. So the groups, in order, are the runs but the last: each group's heights must
/// follow the last's, its command line's chain parameters must be its run's, and the groups'
/// heights must be the extraction's unchecked ones (`bip9.unchecked`). The last run takes the
/// rest, to the tip, and must connect a block.
pub fn segments_in(
    name: &str,
    tip: usize,
    extract: &Value,
    chainargs: &Value,
) -> Result<Vec<Segment>, String> {
    let strings = |v: &Value| -> Result<Vec<String>, String> {
        let list: Option<Vec<String>> =
            v.as_array().and_then(|l| l.iter().map(|a| a.as_str().map(str::to_owned)).collect());
        list.ok_or(format!("{name}: not a list of strings: {v}"))
    };
    let chain_args = strings(&extract["chain_args"])?;

    let starts = chainargs["starts"].as_array().ok_or(format!("{name}: no starts"))?;
    let last = starts.last().ok_or(format!("{name}: no starts"))?;
    let mut runs: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    for (i, start) in starts.iter().enumerate() {
        let (cmdline, config) =
            start_chain_args(start).map_err(|e| format!("{name}: start {i}: {e}"))?;
        let all = [&cmdline[..], &config[..]].concat();
        if but_vbparams(&all) != but_vbparams(&chain_args) {
            return Err(format!(
                "{name}: start {i} runs {all:?}, which differs from chain_args {chain_args:?} \
                 in more than -vbparams"
            ));
        }
        if runs.last().is_none_or(|(_, run)| *run != all) {
            runs.push((cmdline, all));
        }
    }
    let final_args = &runs.last().expect("a start").1;
    if *final_args != chain_args {
        return Err(format!(
            "{name}: the last start runs {final_args:?}, but chain_args is {chain_args:?}"
        ));
    }
    if let Some(i) = starts.iter().position(|start| start["config"] != last["config"]) {
        return Err(format!("{name}: the config of start {i} is not the last start's"));
    }

    let groups = extract["bip9"]["groups"].as_array().ok_or(format!("{name}: no bip9 groups"))?;
    if groups.len() + 1 != runs.len() {
        return Err(format!(
            "{name}: {} bip9 groups, but the starts ran {} argument sets: {:?}",
            groups.len(),
            runs.len(),
            runs.iter().map(|r| &r.1).collect::<Vec<_>>()
        ));
    }
    let mut out: Vec<Segment> = Vec::new();
    let mut checked = Vec::new();
    for (group, (cmdline, args)) in groups.iter().zip(&runs) {
        let heights = heights_of_list(&group["checked"]);
        let from = out.last().map_or(0, |s| s.to + 1);
        let to = from + heights.len().max(1) - 1;
        if heights != (from..=to).collect::<Vec<_>>() {
            return Err(format!("{name}: a group's heights {heights:?} do not follow {from}"));
        }
        let mut ran = Vec::new();
        for arg in strings(&group["args"])? {
            let (k, v) = arg.split_once('=').unwrap_or((&arg, ""));
            let k = k.strip_prefix('-').ok_or(format!("{name}: a group's {arg}"))?;
            chain_param(k, &[v], &mut ran).map_err(|e| format!("{name}: a group: {e}"))?;
        }
        if ran != *cmdline {
            return Err(format!("{name}: a group ran {ran:?}, its starts {cmdline:?}"));
        }
        checked.extend(heights);
        out.push(Segment {
            from,
            to,
            args: args.clone(),
        });
    }
    let from = out.last().map_or(0, |s| s.to + 1);
    if from > tip {
        return Err(format!("{name}: the groups reach the tip, the last start connected nothing"));
    }
    out.push(Segment {
        from,
        to: tip,
        args: chain_args,
    });
    let unchecked: Vec<usize> = heights_of_list(&extract["bip9"]["unchecked"]);
    if unchecked != checked {
        return Err(format!(
            "{name}: groups checked {checked:?}, the extraction left {unchecked:?}"
        ));
    }
    Ok(out)
}

/// A JSON list of heights.
fn heights_of_list(v: &Value) -> Vec<usize> {
    v.as_array()
        .expect("a list of heights")
        .iter()
        .map(|h| usize::try_from(h.as_u64().unwrap()).unwrap())
        .collect()
}

/// What [`rejudge`] did with one recording: its starts, the blocks it submitted, and the
/// segment ends whose deployment states it compared, with the differences it found.
pub struct Rejudged {
    pub starts: usize,
    pub submitted: usize,
    pub observed: Vec<usize>,
    pub differences: Vec<String>,
}

/// Replays `rec`'s blocks after genesis into a fresh dashd rc.5 in `dir`, one start per segment
/// under its arguments at the recording's clock, each start pinned ([`pinned`]): every
/// `submitblock` answers null and makes the block the tip. The first other answer is the error,
/// with the block, its height and its segment (C6). At each segment's last block, before the
/// restart, dashd's deployment states there are compared with the golden's
/// ([`deployment_problems`]); the differences are returned, the replay goes on.
pub fn rejudge(
    binary: &Path,
    dir: &Path,
    rec: &Recording,
    segs: &[Segment],
) -> Result<Rejudged, String> {
    let (clock, _) = rec.clock();
    let golden = rec.golden();
    let mut done = Rejudged {
        starts: 0,
        submitted: 0,
        observed: Vec::new(),
        differences: Vec::new(),
    };
    for seg in segs {
        let args: Vec<&str> = seg.args.iter().map(String::as_str).collect();
        let dashd = Dashd::start_at(binary, dir, clock, &args);
        pinned(&dashd);
        done.starts += 1;
        let tip = rpc(&dashd, "getblockcount", json!([]));
        let expected = seg.from.max(1) - 1;
        if tip != json!(expected) {
            return Err(format!("{}: segment {seg:?} starts at tip {tip}", rec.name));
        }
        for h in seg.from.max(1)..=seg.to {
            let block = &rec.blocks[h];
            let answer = rpc(&dashd, "submitblock", json!([hex(&core_serialize(&**block))]));
            if answer != Value::Null {
                return Err(format!(
                    "C6, a finding: {}: dashd answers {answer} to block {h} ({}) in the segment \
                     from {} under {:?}",
                    rec.name,
                    block.hash(),
                    seg.from,
                    seg.args
                ));
            }
            let best = rpc(&dashd, "getbestblockhash", json!([]));
            if best != json!(block.hash().to_string()) {
                return Err(format!("{}: block {h} accepted, but the tip is {best}", rec.name));
            }
            done.submitted += 1;
        }
        let end = seg.to;
        let record = golden.iter().find(|r| r["h"] == json!(end));
        let record = record.ok_or(format!("{}: no golden C record at {end}", rec.name))?;
        let hash = rec.blocks[end].hash().to_string();
        let observed = rpc(&dashd, "getdeploymentinfo", json!([hash]));
        let problems = deployment_problems(end, &observed, &record["s"]["dep"]);
        done.differences.extend(problems.into_iter().map(|p| format!("{}: {p}", rec.name)));
        done.observed.push(end);
    }
    Ok(done)
}

/// Core's BIP9 deployments at rc.5 in the golden's `s.dep` order, as the recorder's extraction
/// check names them (`conformance/src/digest/extract/checks.rs:565`).
pub const DEP_NAMES: [&str; 3] = ["testdummy", "v24", "evo_shares"];

/// Where dashd's deployment states at block `h` (`observed`, its `getdeploymentinfo` there)
/// differ from the golden's `s.dep` at `h` (`[name, state, since]` each), read as the recorder's
/// extraction check reads them (`conformance/src/digest/extract/checks.rs:355-378`): each
/// deployment's state is the child's (`bip9.status_next`, `rpc/blockchain.cpp:1504`), since the
/// height `since_next` derives (`checks.rs:576-597`): `since` where the child keeps the block's
/// state, else `h + 1`, and the RPC's `height` where it gives one must agree; a deployment the
/// RPC does not list is disabled (`rpc/blockchain.cpp:1464`; a never-active `-vbparams`), failed
/// since 0. A BIP9 deployment the RPC lists and the golden does not is a difference too. The
/// golden must name Core's BIP9 deployments exactly, in order ([`DEP_NAMES`], `checks.rs:332-345`),
/// so a deployment dropped from both sides is not left unchecked.
pub fn deployment_problems(h: usize, observed: &Value, golden: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let Some(recorded) = golden.as_array() else {
        return vec![format!("{h}: the golden's s.dep is {golden}")];
    };
    let listed: Vec<Option<&str>> = recorded.iter().map(|e| e[0].as_str()).collect();
    if listed != DEP_NAMES.map(Some) {
        problems.push(format!("{h}: the golden's s.dep names {listed:?}, not {DEP_NAMES:?}"));
    }
    let deployments = &observed["deployments"];
    if !deployments.is_object() {
        problems.push(format!("{h}: dashd's getdeploymentinfo has no deployments"));
    }
    let mut names = BTreeSet::new();
    for entry in recorded {
        let (Some(name), want) = (entry[0].as_str(), (&entry[1], &entry[2])) else {
            problems.push(format!("{h}: a golden s.dep entry {entry}"));
            continue;
        };
        names.insert(name);
        let got = match deployments.get(name) {
            None => (json!("failed"), json!(0)),
            Some(d) if d["type"] == "bip9" => {
                let b = &d["bip9"];
                let since = if b["status_next"] == b["status"] {
                    b["since"].clone()
                } else {
                    json!(h + 1)
                };
                if d.get("height").is_some_and(|height| *height != since) {
                    problems
                        .push(format!("{h}: {name}'s height {} but since {since}", d["height"]));
                }
                (b["status_next"].clone(), since)
            }
            Some(d) => (json!(format!("not bip9: {d}")), Value::Null),
        };
        if (&got.0, &got.1) != want {
            problems.push(format!(
                "{h}: {name} is ({}, {}) on dashd, ({}, {}) in the golden",
                got.0, got.1, want.0, want.1
            ));
        }
    }
    if let Some(all) = deployments.as_object() {
        for (name, d) in all {
            if d["type"] == "bip9" && !names.contains(name.as_str()) {
                problems.push(format!("{h}: dashd lists {name}, the golden does not"));
            }
        }
    }
    problems
}

/// Every stored recording's name whose node0 chain carries a type 7 or 9 tx.
pub fn carrying() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for entry in std::fs::read_dir(RECORDINGS_DIR).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if !Path::new(RECORDINGS_DIR).join(&name).join("node0/blocks.dat.zst").exists() {
            continue;
        }
        let rec = Recording::open(&name);
        if !rec.heights_of(7).is_empty() || !rec.heights_of(9).is_empty() {
            found.insert(name);
        }
    }
    found
}

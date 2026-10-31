//! The CRASH model's synthetic blocks (CH2 §6.4): real blocks encoded with CS0 and undo data
//! with CS5a, a function of (parent, height, branch) so that the system under test and its
//! reference generate the same chain.
//!
//! Block h on branch b holds:
//! - a coinbase with a spendable output and an unspendable one (`script_sig` names h and b);
//! - for h ≥ 2, A: it spends the parent's coinbase output into a short and a long output;
//! - for h ≥ 3, B: it spends the parent's A.0 into a short and a long output;
//! - in an invalid block, C: it spends an outpoint no block creates, so `HaveInputs` fails.
//!
//! So every valid block spends exactly two coins its parent created, and its undo data follows
//! from the parent alone. Genesis's outputs are never in the UTXO set, as Core's are not. Each
//! header's time is `T0 + b`, so a block read back from disk names its branch.
//!
//! Core mode (`connect=core`, CH3a a3) generates blocks the real `ConnectBlock` connects on
//! regtest instead ([`generate`]'s `core`): regtest's genesis, then block h on branch b holds
//! - a coinbase whose `scriptSig` starts with BIP34's height, paying `OP_TRUE`, an `OP_RETURN`
//!   naming b, and a pay-to-pubkey output;
//! - for h ≥ 101, A: it spends the `OP_TRUE` output of the coinbase at h − 100, just mature
//!   (`COINBASE_MATURITY`);
//! - when the parent has A, B: it spends the parent's A.0 (`OP_TRUE`, no maturity);
//! - in an invalid block, C: for h ≥ 101 it spends the pay-to-pubkey output of the coinbase at
//!   h − 100 with a well-formed signature that does not verify, so the script check fails
//!   (`mandatory-script-verify-flag-failed`); below, with no mature coin, an outpoint no block
//!   creates (`bad-txns-inputs-missingorspent`).
//!
//! Its undo data is what `ConnectBlock` streams, and R1 compares the two.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use dash_consensus::coins::compress::is_unspendable;
use dash_consensus::coins::{BlockUndo, Coin, TxUndo};
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::{
    Block, BlockHash, BlockHeader, Header, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn,
    TxOut, TxType, core_bytes,
};
use dash_num::CompactTarget;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dashd_store::flatfile::{CoreBytes, CoreUndoBytes, UNDO_CHECKSUM_SIZE};

use crate::coins::{CoinSrc, OutPointKey};

/// Regtest's limit, `0x207fffff`: about every other hash meets it.
pub const REGTEST_BITS: u32 = 0x207f_ffff;
/// The header time of branch 0's blocks.
pub const T0: u32 = 1_700_000_000;

/// One generated block.
pub struct GenBlock {
    pub block: Block,
    pub hash: BlockHash,
    /// All zero for genesis.
    pub parent: BlockHash,
    pub height: u32,
    pub branch: u8,
    pub bytes: CoreBytes,
    /// Empty for genesis, which has none.
    pub undo: CoreUndoBytes,
    pub valid: bool,
    /// The spendable outputs it creates, with their coins as stored.
    pub adds: Vec<(OutPointKey, Vec<u8>)>,
    /// The coins it spends, with their coins as stored (the undo data's).
    pub spends: Vec<(OutPointKey, Vec<u8>)>,
}

/// A block-file record's header (magic and length) before its payload.
pub const RECORD_HEADER: u32 = 8;

impl GenBlock {
    /// The block record's length in `blk`: its header and the block's bytes.
    pub fn data_len(&self) -> u64 {
        u64::from(RECORD_HEADER) + self.bytes.as_slice().len() as u64
    }

    /// The undo record's length in `rev`: its header, the undo bytes and the checksum.
    pub fn undo_len(&self) -> u64 {
        u64::from(RECORD_HEADER) + self.undo.as_slice().len() as u64 + u64::from(UNDO_CHECKSUM_SIZE)
    }

    /// The coinbase's spendable output: what the child's A spends.
    pub(super) fn coinbase_out(&self) -> (OutPointKey, Vec<u8>) {
        self.output(0)
    }

    /// A's short output: what the child's B spends.
    fn a_out(&self) -> Option<(OutPointKey, Vec<u8>)> {
        (self.block.txs().len() > 1 && self.height >= 2).then(|| self.output(1))
    }

    fn output(&self, tx: usize) -> (OutPointKey, Vec<u8>) {
        let t = &self.block.txs()[tx];
        (out_key(t, 0), pack(&t.outputs()[0], self.height, tx == 0))
    }
}

/// The key of output `o` of `tx`.
pub(super) fn out_key(tx: &Tx, o: usize) -> OutPointKey {
    OutPointKey::new(&OutPoint {
        hash: tx.txid(),
        index: o as u32,
    })
}

/// A coin as the coins table stores it: Core's `Coin` serialization.
pub fn pack(out: &TxOut, height: u32, coinbase: bool) -> Vec<u8> {
    let src = CoinSrc::Output {
        out,
        height,
        coinbase,
    };
    src.pack().expect("a valid output").expect("a spendable output").as_bytes().to_vec()
}

/// (parent, height, branch, invalid, core mode).
type Key = (BlockHash, u32, u8, bool, bool);

/// Every block generated in this process: each is mined once (the explorer reopens the system
/// thousands of times).
static BLOCKS: LazyLock<Mutex<HashMap<Key, Arc<GenBlock>>>> = LazyLock::new(Default::default);
/// The same blocks by hash, for [`lookup`].
static BY_HASH: LazyLock<Mutex<HashMap<BlockHash, Arc<GenBlock>>>> =
    LazyLock::new(Default::default);

/// A block this process generated, by hash: its encoded sizes are what the extractor's
/// requirements cover (the toy's fixed sizes).
pub fn lookup(hash: &BlockHash) -> Option<Arc<GenBlock>> {
    BY_HASH.lock().expect("unpoisoned").get(hash).cloned()
}

/// Every block this process generated, by hash: a reference's blocks among them.
pub fn generated() -> HashMap<BlockHash, Arc<GenBlock>> {
    BY_HASH.lock().expect("unpoisoned").clone()
}

/// The block at `height` on `branch` after `parent` (`None` for genesis), invalid as asked; in
/// core mode (`core`) one the real `ConnectBlock` judges.
pub fn generate(
    parent: Option<&GenBlock>,
    height: u32,
    branch: u8,
    invalid: bool,
    core: bool,
) -> Arc<GenBlock> {
    let key = (parent.map_or(BlockHash::from([0; 32]), |p| p.hash), height, branch, invalid, core);
    if let Some(b) = BLOCKS.lock().expect("unpoisoned").get(&key) {
        return b.clone();
    }
    let b = Arc::new(match (core, parent) {
        (false, _) => build(parent, height, branch, invalid),
        (true, None) => core_genesis(),
        (true, Some(p)) => build_core(p, height, branch, invalid),
    });
    // Both maps under `BLOCKS`'s lock: a block another thread finds in `BLOCKS` is in `BY_HASH`,
    // where core mode's generator looks up its ancestors.
    let mut blocks = BLOCKS.lock().expect("unpoisoned");
    let b = blocks.entry(key).or_insert(b).clone();
    BY_HASH.lock().expect("unpoisoned").entry(b.hash).or_insert_with(|| b.clone());
    b
}

fn build(parent: Option<&GenBlock>, height: u32, branch: u8, invalid: bool) -> GenBlock {
    debug_assert_eq!(parent.map_or(0, |p| p.height + 1), height);
    let mut txs = vec![coinbase(height, branch)];
    let mut spends = Vec::new();
    if let Some(p) = parent.filter(|_| height >= 2) {
        let cb = p.coinbase_out();
        txs.push(spend(cb.0.outpoint(), height, branch, 0));
        spends.push(cb);
        if let Some(a) = p.a_out() {
            txs.push(spend(a.0.outpoint(), height, branch, 1));
            spends.push(a);
        }
    }
    if invalid {
        let missing = OutPoint {
            hash: TxHash::from([0xee; 32]),
            index: height,
        };
        txs.push(spend(missing, height, branch, 2));
    }
    let prev = parent.map_or(BlockHash::from([0; 32]), |p| p.hash);
    let header = BlockHeader {
        version: 4,
        prev_hash: prev,
        merkle_root: MerkleRoot::from([0; 32]),
        time: T0 + u32::from(branch),
        bits: REGTEST_BITS,
        nonce: 0,
    };
    let block = mine(header, txs);
    let mut adds = Vec::new();
    if height > 0 {
        for (i, tx) in block.txs().iter().enumerate() {
            for (o, out) in tx.outputs().iter().enumerate() {
                // The coinbase's second output is `OP_RETURN`.
                if i == 0 && o == 1 {
                    continue;
                }
                adds.push((out_key(tx, o), pack(out, height, i == 0)));
            }
        }
    }
    let undo = if height == 0 {
        CoreUndoBytes::from_raw(Vec::new())
    } else {
        undo_of(&spends)
    };
    GenBlock {
        hash: block.hash(),
        bytes: core_bytes(&block),
        block,
        parent: prev,
        height,
        branch,
        undo,
        valid: !invalid,
        adds,
        spends,
    }
}

/// `txs` under `header` and their merkle root, at the first nonce whose hash meets regtest's
/// limit.
pub(super) fn mine(mut header: BlockHeader, txs: Vec<Arc<Tx>>) -> Block {
    (header.merkle_root, _) = Block::new(Header::new(header), txs.clone()).merkle();
    loop {
        let block = Block::new(Header::new(header), txs.clone());
        if check_proof_of_work(block.hash().as_bytes(), CompactTarget::new(REGTEST_BITS), &pow()) {
            return block;
        }
        header.nonce += 1;
    }
}

/// Regtest's parameters (v24), core mode's network.
pub fn regtest() -> Arc<ChainSetup> {
    static PARAMS: LazyLock<Arc<ChainSetup>> = LazyLock::new(|| {
        let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build();
        Arc::new(params.expect("regtest's parameters"))
    });
    PARAMS.clone()
}

/// Regtest's proof-of-work parameters, which every generated block meets.
pub fn pow() -> PowParams {
    static POW: LazyLock<PowParams> =
        LazyLock::new(|| PowParams::from_chain_params(&regtest()).expect("regtest's"));
    *POW
}

/// A version-2 transaction of one input.
fn tx(input: TxIn, outputs: Vec<TxOut>, lock_time: u32) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: vec![input],
        outputs,
        lock_time,
        extra_payload: Vec::new(),
    }))
}

fn txout(value: i64, script_pubkey: Vec<u8>) -> TxOut {
    TxOut {
        value,
        script_pubkey: script_pubkey.into(),
    }
}

/// A coinbase with a spendable output and an unspendable one.
fn coinbase(height: u32, branch: u8) -> Arc<Tx> {
    let mut script_sig = height.to_le_bytes().to_vec();
    script_sig.push(branch);
    let input = TxIn {
        prevout: OutPoint {
            hash: TxHash::from([0; 32]),
            index: u32::MAX,
        },
        script_sig: script_sig.into(),
        sequence: u32::MAX,
    };
    let outputs =
        vec![txout(50_000 + i64::from(height), vec![0x76; 25]), txout(0, vec![0x6a, branch])];
    tx(input, outputs, 0)
}

/// A transaction spending `input` into a short output and a long one.
fn spend(input: OutPoint, height: u32, branch: u8, i: u8) -> Arc<Tx> {
    let input = TxIn {
        prevout: input,
        script_sig: Default::default(),
        sequence: u32::MAX,
    };
    let outputs = vec![txout(1_000, vec![0x51; 25]), txout(2_000, vec![0x52; 80])];
    tx(input, outputs, height << 16 | u32::from(branch) << 8 | u32::from(i))
}

// ---------------------------------------------------------------------------------------------
// Core mode.

// Core: consensus/consensus.h:19 COINBASE_MATURITY
/// A coinbase output is spendable this many blocks on.
pub(super) const COINBASE_MATURITY: u32 = 100;

/// `OP_TRUE`: core mode's spendable outputs, spent with an empty `scriptSig`.
pub(super) const OP_TRUE: u8 = 0x51;

/// Core mode's pay-to-pubkey script: secp256k1's generator, compressed, then `OP_CHECKSIG`.
const P2PK: [u8; 35] = [
    0x21, 0x02, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87,
    0x0b, 0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8,
    0x17, 0x98, 0xac,
];

/// C's `scriptSig`: one push of a strict-DER signature (r = s = 1, low S) with `SIGHASH_ALL`,
/// which passes every encoding check and verifies for no message.
const BAD_SIG_PUSH: [u8; 10] = [0x09, 0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01, 0x01];

/// Regtest's genesis block: Core's own, whose outputs never enter the UTXO set.
fn core_genesis() -> GenBlock {
    let g = crate::chain::genesis_block(&regtest());
    let block = Block::new(*g.header(), g.txs().to_vec());
    GenBlock {
        hash: block.hash(),
        bytes: core_bytes(&block),
        block,
        parent: BlockHash::from([0; 32]),
        height: 0,
        branch: 0,
        undo: CoreUndoBytes::from_raw(Vec::new()),
        valid: true,
        adds: Vec::new(),
        spends: Vec::new(),
    }
}

/// The generated ancestor of `parent` at `height`.
fn ancestor(parent: &GenBlock, height: u32) -> Arc<GenBlock> {
    let mut at = lookup(&parent.hash).expect("a generated parent");
    while at.height > height {
        at = lookup(&at.parent).expect("a generated ancestor");
    }
    at
}

fn build_core(parent: &GenBlock, height: u32, branch: u8, invalid: bool) -> GenBlock {
    debug_assert_eq!(parent.height + 1, height);
    let mut txs = vec![core_coinbase(height, branch)];
    let mut spends = Vec::new();
    let matured = height.checked_sub(COINBASE_MATURITY).filter(|h| *h >= 1);
    let matured = matured.map(|h| ancestor(parent, h));
    if let Some(m) = &matured {
        let cb = m.coinbase_out();
        let outs = vec![txout(1_000, vec![OP_TRUE]), txout(2_000, vec![0x52; 80])];
        txs.push(core_spend(cb.0.outpoint(), Vec::new(), outs, height, branch, 0));
        spends.push(cb);
    }
    if let Some(a) = parent.a_out() {
        let outs = vec![txout(300, vec![OP_TRUE]), txout(300, vec![0x53; 40])];
        txs.push(core_spend(a.0.outpoint(), Vec::new(), outs, height, branch, 1));
        spends.push(a);
    }
    if invalid {
        let outs = vec![txout(500, vec![OP_TRUE])];
        let c = match &matured {
            Some(m) => {
                let tx = &m.block.txs()[0];
                core_spend(
                    out_key(tx, 2).outpoint(),
                    BAD_SIG_PUSH.to_vec(),
                    outs,
                    height,
                    branch,
                    2,
                )
            }
            None => {
                let missing = OutPoint {
                    hash: TxHash::from([0xee; 32]),
                    index: height,
                };
                core_spend(missing, Vec::new(), outs, height, branch, 2)
            }
        };
        txs.push(c);
    }
    let header = BlockHeader {
        version: 4,
        prev_hash: parent.hash,
        merkle_root: MerkleRoot::from([0; 32]),
        time: T0 + u32::from(branch),
        bits: REGTEST_BITS,
        nonce: 0,
    };
    let block = mine(header, txs);
    // Core: coins.cpp:118-128 AddCoins (`AddCoin` skips an unspendable output)
    let adds = block
        .txs()
        .iter()
        .enumerate()
        .flat_map(|(i, tx)| {
            tx.outputs()
                .iter()
                .enumerate()
                .filter(|(_, out)| !is_unspendable(out.script_pubkey.as_bytes()))
                .map(move |(o, out)| (out_key(tx, o), pack(out, height, i == 0)))
        })
        .collect();
    GenBlock {
        hash: block.hash(),
        bytes: core_bytes(&block),
        block,
        parent: parent.hash,
        height,
        branch,
        undo: undo_of(&spends),
        valid: !invalid,
        adds,
        spends,
    }
}

/// The undo data of a block spending `spends`, one per transaction after the coinbase.
fn undo_of(spends: &[(OutPointKey, Vec<u8>)]) -> CoreUndoBytes {
    let txs = spends
        .iter()
        .map(|(_, coin)| TxUndo {
            coins: vec![Coin::decode(&mut &coin[..]).expect("a stored coin decodes")],
        })
        .collect();
    CoreUndoBytes::from_raw(
        BlockUndo {
            txs,
        }
        .core_bytes()
        .as_slice(),
    )
}

/// Core mode's coinbase: BIP34's height and the branch in its `scriptSig`.
fn core_coinbase(height: u32, branch: u8) -> Arc<Tx> {
    let mut script_sig = height_push(height);
    script_sig.extend([1, branch]);
    let input = TxIn {
        prevout: OutPoint {
            hash: TxHash::from([0; 32]),
            index: u32::MAX,
        },
        script_sig: script_sig.into(),
        sequence: u32::MAX,
    };
    let outputs = vec![
        txout(50_000 + i64::from(height), vec![OP_TRUE]),
        txout(0, vec![0x6a, branch]),
        txout(1_000, P2PK.to_vec()),
    ];
    tx(input, outputs, 0)
}

// Core: script/script.h:427 CScript::push_int64
/// `CScript() << n`, BIP34's coinbase height: `OP_1`…`OP_16`, else the minimal `CScriptNum`
/// push.
pub(super) fn height_push(n: u32) -> Vec<u8> {
    match n {
        0 => vec![0x00],
        1..=16 => vec![0x50 + n as u8],
        _ => {
            let mut num = n.to_le_bytes().to_vec();
            while num.last() == Some(&0) {
                num.pop();
            }
            if num.last().is_some_and(|b| b & 0x80 != 0) {
                num.push(0);
            }
            let mut out = vec![num.len() as u8];
            out.extend(num);
            out
        }
    }
}

/// A transaction spending `input` with `script_sig` into `outputs`.
fn core_spend(
    input: OutPoint,
    script_sig: Vec<u8>,
    outputs: Vec<TxOut>,
    height: u32,
    branch: u8,
    i: u8,
) -> Arc<Tx> {
    let input = TxIn {
        prevout: input,
        script_sig: script_sig.into(),
        sequence: u32::MAX,
    };
    tx(input, outputs, height << 16 | u32::from(branch) << 8 | u32::from(i))
}

//! `connect_block_micro` (CH3 §6.3; CH3a a3, a4): `ConnectBlock` of eight fixed mainnet-shaped
//! blocks against a warm in-memory coins cache, at `-par=1` and `-par=4` and at `-assumevalid` off
//! and on, gated on instructions and allocations by PF0's counter gate
//! (`crit:dashd-chainstate connect_block_micro`).
//!
//! Rows are `serial/<block>/av-{off,on}` (`-par=1`: `CheckQueue::serial()`, every input checked
//! inline) and `par4/<block>/av-{off,on}` (`-par=4`: a `CheckQueue` with three threads beside the
//! bench thread, so an `av-off` block's input checks are queued and its region ends with the
//! control's wait, `v:2649`, `v:2778`). Without script checks Core's control has no queue, so a
//! `par4` `av-on` row is the `serial` one beside three idle threads. PF0 gates ten serial rows
//! (`contrib/node/perfgate/workloads.toml`, CH3a D78): both sides of the two script shapes
//! (`p2pkh-500`, `p2sh-multisig-250`), one side of the others; and the par4 rows CH3a D131 names.
//!
//! - `empty`: a coinbase-only block.
//! - `p2pkh-500`: 500 P2PKH inputs, two per transaction, two P2PKH outputs each.
//! - `p2sh-multisig-250`: 250 P2SH 2-of-3 multisig inputs (two signatures each), two per
//!   transaction, paying P2SH outputs.
//! - `pre-dip1-1mb`: P2PKH transactions as `p2pkh-500`'s up to 1 MB, below DIP0001.
//! - `dip1-2mb`: the same up to 2 MB, with DIP0001 active (2 MB blocks, 40,000 sigops).
//! - `chained-200`: 200 one-input transactions, each spending the one before it in the block.
//! - `bad-txns-inputs-missingorspent`: `p2pkh-500` whose last input's coin does not exist: the
//!   reject comes at the last transaction's `CheckTxInputs`.
//! - `bad-cb-amount`: `p2pkh-500` whose coinbase pays one duff more than the subsidy and the fees:
//!   the reject comes after every input's scripts, before the undo write.
//!
//! `av-off` is `-assumevalid=0`, so every input's scripts are checked (the signature and script
//! caches are consulted and miss, as on a block the mempool never saw: `ConnectBlock` stores
//! nothing in them). `av-on` names the best header as the assumed-valid block, 10,000 headers
//! past the block (more than two weeks of equivalent work, Core's `v:2542-2571`), so no script
//! is checked, and connects the block unsigned: [`PLACEHOLDER`] stands in for every signature, at
//! a common signature's length, so a connect that checked a script would reject it. Each row
//! builds its own block.
//!
//! **What a row measures.** One [`SerialCore::connect`]: Core's `ConnectBlock`
//! (`validation.cpp:2469-2935`) with `fChecked` set (the block `ProcessNewBlock` checked), during
//! IBD (no InstantSend conflict loop), the evo side as `NoEvo` answers below DIP0003, into a
//! [`BlockOverlay`] over the warm cache. It includes the undo record's write
//! (`WriteUndoDataForBlock`: its checksum, one `pwrite` into a rev file on disk, and the 1 MiB
//! chunk's `posix_fallocate` whenever the record crosses one) and the block entry's status writes, and the
//! overlay's drop. It leaves out V's bracket (`CoreConnector::connect_block`: the EV attempt and
//! the overlay's commit), which needs V's `VContext` (EV addendum §2.6), so no bench can open it;
//! the commit is CH2's `coins-overlay-block-1k` row.
//!
//! **Every iteration starts from the same state.** The overlay is dropped uncommitted, so the
//! cache is untouched; each iteration's setup (outside the region, `BatchSize::PerIteration`)
//! puts the block's entry back to accepted-not-connected (`BLOCK_VALID_TRANSACTIONS` with data,
//! no undo) and drains the index's dirty set, and starts the rev file over, empty, once its
//! records pass 16 MiB. One unmeasured connect per row first warms the core's buffers and the versionbits
//! cache, and checks the outcome; every measured one checks it again.
//!
//! **The blocks and their chain.** No mainnet block and its spent coins are in the repository,
//! so the blocks are built here, deterministically: fixed keys (`tests/support/signed.rs`),
//! libsecp256k1's RFC 6979 signatures, fixed outpoints. They sit at mainnet heights under
//! mainnet's parameters: 630,000 (after CSV, before DIP0001; P2SH, DERSIG, CLTV and CSV
//! enforced), and 790,000 for the 2 MB block (DIP0001 active, before BIP147). Neither is a
//! superblock or budget height. Their chain is dash-chain's bench fixture's: Core's genesis, then
//! synthetic headers at the genesis `nBits` whose stored hashes meet it (the index load never
//! hashes), with mainnet's BIP34 block at 951 (so BIP30 is skipped, as on mainnet), the block,
//! and 10,000 headers after it. The block's own header is mined at the genesis `nBits` (about
//! 2^20 X11 hashes); [`NONCES`] records each nonce so the setup hashes once. Each process's
//! setup loads the 640,000- or 800,000-entry index (about 4–5 G instructions) and, for an `av-off`
//! row, signs the block's inputs (about 0.7 M instructions a signature: 7.7 G for `dip1-2mb`'s
//! 10,700), outside the region.

// PROCESS.md: benches measure production only, so a test feature turned on here (by a self
// dev-dependency or `--all-features`) fails the build. Clippy's `--all-features` run is exempt.
#[cfg(all(not(clippy), any(feature = "crash", feature = "planted-bugs")))]
compile_error!("benches measure production only: build them without `crash` and `planted-bugs`");

#[path = "../tests/support/signed.rs"]
mod signed;

use std::cell::RefCell;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_chain::{
    BlockIndexRecord, ChainConfig, ChainReader, CompleteAncestry, FailStop, HeaderChain, LoadError,
    NoStore, Services, StaticSporks, Status, Work,
};
use dash_clock::MockClock;
use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_consensus::hash::hash160;
use dash_consensus::header::HeaderParams;
use dash_consensus::pow::check_proof_of_work;
use dash_consensus::primitives::{
    BlockHash, BlockHeader, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType,
};
use dash_consensus::script::{SIGHASH_ALL, push_data, signature_hash};
use dash_consensus::subsidy::{COIN, block_subsidy};
use dash_consensus::{Amount, Block, BlockId, BlockValidationResult, Header, HeaderCtx, Tx};
use dash_evo::payments::PaymentsParams;
use dash_num::CompactTarget;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{
    AbortPath, AttemptOrigin, ChainLockView, ConnectCx, ConnectOutcome, Fatal, GuardScope,
    RecordingAbort,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{
    BlockOverlay, BlockRef, CoinSrc, CoinsBudget, CoinsCache, OutPointKey, Ratio, Salt,
};
use dashd_chainstate::connect::bench::{NoEvoSteps, status_writer};
use dashd_chainstate::connect::{NodeHelper, SerialCore};
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches, ScriptExecCache, SigCache};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, RecordingSink,
};
use dashd_store::MemoryEngine;
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};
use signed::{p2pkh, pubkey, sign_all};
use tempfile::TempDir;

/// After CSV (622,944), before DIP0001 (782,208); not a superblock height.
const PRE_DIP1: u32 = 630_000;
/// After DIP0001, before BIP147 (939,456); not a superblock height.
const DIP1: u32 = 790_000;
/// Headers after the block: the best header's lead, past two weeks of equivalent work.
const AV_LEAD: u32 = 10_000;
/// How deep the spent coins are.
const COIN_AGE: u32 = 1_000;
const FEE: Amount = 10_000;
const VERSION: i32 = 0x2000_0000;
/// The rev file starts over past this, outside the region.
const ROTATE: u64 = 16 << 20;
const MIB: u64 = 1 << 20;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Shape {
    Empty,
    P2pkh500,
    Multisig250,
    PreDip1Mb,
    Dip1TwoMb,
    Chained200,
    BadMissing,
    BadCbAmount,
}

/// Each block's header nonce at the genesis `nBits` (see [`mine`]), in [`Shape::ALL`]'s order:
/// the signed block's (`av-off`), then the unsigned one's (`av-on`).
const NONCES: [[u32; 2]; 8] = [
    [2_483_076, 2_483_076],
    [867_405, 1_670_435],
    [875_359, 382_631],
    [21_492, 1_786_630],
    [1_895_189, 633_361],
    [54_975, 1_668_470],
    [359_154, 1_624_611],
    [1_934_047, 1_397_993],
];

/// `-par`: the check queue a row's core runs on (CH3 §2.9).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Par {
    /// `-par=1`: no threads.
    Serial,
    /// `-par=4`: three threads beside the caller (`script_check_threads(4, n)` for n ≥ 4).
    Par4,
}

impl Par {
    fn name(self) -> &'static str {
        match self {
            Par::Serial => "serial",
            Par::Par4 => "par4",
        }
    }

    fn queue(self) -> CheckQueue {
        match self {
            Par::Serial => CheckQueue::serial(),
            Par::Par4 => CheckQueue::new(3).expect("three script-check threads"),
        }
    }
}

/// `-assumevalid` off (`=0`: every script checked, on a signed block) or on (the best header
/// assumed valid: no script checked, on an unsigned block).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Av {
    Off = 0,
    On = 1,
}

impl Av {
    fn name(self) -> &'static str {
        match self {
            Av::Off => "av-off",
            Av::On => "av-on",
        }
    }

    /// Whether the row's block carries real signatures.
    fn signed(self) -> bool {
        matches!(self, Av::Off)
    }
}

impl Shape {
    pub(crate) const ALL: [Shape; 8] = [
        Shape::Empty,
        Shape::P2pkh500,
        Shape::Multisig250,
        Shape::PreDip1Mb,
        Shape::Dip1TwoMb,
        Shape::Chained200,
        Shape::BadMissing,
        Shape::BadCbAmount,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Shape::Empty => "empty",
            Shape::P2pkh500 => "p2pkh-500",
            Shape::Multisig250 => "p2sh-multisig-250",
            Shape::PreDip1Mb => "pre-dip1-1mb",
            Shape::Dip1TwoMb => "dip1-2mb",
            Shape::Chained200 => "chained-200",
            Shape::BadMissing => "bad-txns-inputs-missingorspent",
            Shape::BadCbAmount => "bad-cb-amount",
        }
    }

    fn height(self) -> u32 {
        match self {
            Shape::Dip1TwoMb => DIP1,
            _ => PRE_DIP1,
        }
    }

    pub(crate) fn expect(self) -> Expect {
        match self {
            Shape::BadMissing => {
                Expect::Invalid(BlockValidationResult::Consensus, "bad-txns-inputs-missingorspent")
            }
            Shape::BadCbAmount => Expect::Invalid(BlockValidationResult::Unset, "bad-cb-amount"),
            _ => Expect::Connected,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Expect {
    Connected,
    Invalid(BlockValidationResult, &'static str),
}

impl Expect {
    fn check(self, out: &ConnectOutcome) {
        let ok = match (self, out) {
            (Expect::Connected, ConnectOutcome::Connected) => true,
            (Expect::Invalid(result, reason), ConnectOutcome::Invalid(e)) => {
                e.result() == result && e.reject_reason() == reason
            }
            _ => false,
        };
        assert!(ok, "{out:?}, want {self:?}");
    }
}

// ---- the blocks ------------------------------------------------------------------------------

/// How a spent output is locked, and so unlocked.
#[derive(Clone, Copy)]
enum Lock {
    /// P2PKH to harness key `k`, signed by it.
    P2pkh(usize),
    /// P2SH of a 2-of-3 multisig over keys 0, 1 and 2, signed by keys 0 and 1.
    Multisig,
}

/// The harness keys' public keys and the scripts over them, derived once per process: each
/// public key is a scalar multiplication, which per output would be most of the setup.
struct Scripts {
    /// Keys 0..10: 0–3 lock the spent coins, 8 and 9 the coinbase's outputs.
    pubkeys: Vec<[u8; 33]>,
    p2pkh: Vec<Vec<u8>>,
    /// `OP_2 <key 0> <key 1> <key 2> OP_3 OP_CHECKMULTISIG`.
    redeem: Vec<u8>,
    /// `OP_HASH160 <hash160(redeem)> OP_EQUAL`.
    p2sh: Vec<u8>,
}

static SCRIPTS: LazyLock<Scripts> = LazyLock::new(|| {
    let pubkeys: Vec<[u8; 33]> = (0..10).map(pubkey).collect();
    let mut redeem = vec![0x52];
    for key in &pubkeys[..3] {
        push_data(&mut redeem, key);
    }
    redeem.extend_from_slice(&[0x53, 0xae]);
    let mut p2sh = vec![0xa9, 20];
    p2sh.extend_from_slice(&hash160(&redeem));
    p2sh.push(0x87);
    Scripts {
        p2pkh: pubkeys.iter().map(|key| p2pkh(key)).collect(),
        pubkeys,
        redeem,
        p2sh,
    }
});

impl Lock {
    fn script(self) -> &'static [u8] {
        match self {
            Lock::P2pkh(k) => &SCRIPTS.p2pkh[k],
            Lock::Multisig => &SCRIPTS.p2sh,
        }
    }

    /// Input `i`'s scriptSig in `tx`, whose scriptSigs are all still empty.
    fn unlock(self, tx: &Transaction, i: usize, signed: bool) -> Vec<u8> {
        let sig = |script: &[u8], k: usize| match signed {
            true => sign_all(signature_hash(script, tx, i, SIGHASH_ALL), k),
            false => PLACEHOLDER.to_vec(),
        };
        let mut script_sig = Vec::new();
        match self {
            Lock::P2pkh(k) => {
                push_data(&mut script_sig, &sig(self.script(), k));
                push_data(&mut script_sig, &SCRIPTS.pubkeys[k]);
            }
            Lock::Multisig => {
                let redeem = &SCRIPTS.redeem;
                script_sig.push(0x00);
                push_data(&mut script_sig, &sig(redeem, 0));
                push_data(&mut script_sig, &sig(redeem, 1));
                push_data(&mut script_sig, redeem);
            }
        }
        script_sig
    }
}

/// An unsigned block's signature: DER's shape at a common signature's length (two 32-byte
/// integers, then `SIGHASH_ALL`), which no key made. Only `av-on` connects one: under
/// `-assumevalid` no script runs (`v:2542-2571`), so it connects as the signed block would, and a
/// connect that checked a script would reject it (`mandatory-script-verify-flag-failed`).
const PLACEHOLDER: [u8; 71] = {
    let mut sig = [0x5a; 71];
    (sig[0], sig[1], sig[2], sig[3]) = (0x30, 0x44, 0x02, 0x20);
    (sig[36], sig[37]) = (0x02, 0x20);
    sig[70] = SIGHASH_ALL as u8;
    sig
};

fn output(value: Amount, lock: Lock) -> TxOut {
    TxOut {
        value,
        script_pubkey: lock.script().to_vec().into(),
    }
}

/// A version-1 transaction spending `inputs` into `outputs`, each input signed `SIGHASH_ALL`.
fn spend(inputs: &[(OutPoint, Lock)], outputs: Vec<TxOut>, signed: bool) -> Tx {
    let mut raw = Transaction {
        version: 1,
        tx_type: TxType::new(0),
        inputs: inputs
            .iter()
            .map(|(prevout, _)| TxIn {
                prevout: *prevout,
                script_sig: Vec::new().into(),
                sequence: u32::MAX,
            })
            .collect(),
        outputs,
        lock_time: 0,
        extra_payload: Vec::new(),
    };
    // Every input's signature hash blanks the other scriptSigs, so sign before filling any.
    let script_sigs: Vec<Vec<u8>> =
        inputs.iter().enumerate().map(|(i, (_, lock))| lock.unlock(&raw, i, signed)).collect();
    for (input, script_sig) in raw.inputs.iter_mut().zip(script_sigs) {
        input.script_sig = script_sig.into();
    }
    Tx::new(raw)
}

/// The coin a block spends from the cache: shape-tagged, unique.
fn funding(shape: Shape, n: u32) -> OutPoint {
    let mut hash = [0x5e; 32];
    hash[..4].copy_from_slice(&n.to_le_bytes());
    hash[4] = shape as u8;
    OutPoint {
        hash: TxHash::from(hash),
        index: n % 3,
    }
}

/// A block's transactions after the coinbase, the coins they spend from the cache, their fees.
#[derive(Default)]
struct Spends {
    txs: Vec<Arc<Tx>>,
    seeds: Vec<(OutPoint, TxOut)>,
    fees: Amount,
}

/// Two-input, two-output transactions over coins locked by `lock`, while `more(txs, bytes)`.
fn pairs(
    shape: Shape,
    signed: bool,
    lock: fn(u32) -> Lock,
    more: impl Fn(usize, u64) -> bool,
) -> Spends {
    let mut s = Spends::default();
    let mut bytes = 0;
    let mut n = 0;
    while more(s.txs.len(), bytes) {
        let inputs: Vec<(OutPoint, Lock)> =
            (n..n + 2).map(|m| (funding(shape, m), lock(m))).collect();
        for (op, l) in &inputs {
            s.seeds.push((*op, output(COIN, *l)));
        }
        let value = 2 * COIN - FEE;
        let outputs = vec![output(value / 2, lock(n + 1)), output(value - value / 2, lock(n))];
        let tx = spend(&inputs, outputs, signed);
        bytes += u64::from(tx.size());
        s.txs.push(Arc::new(tx));
        s.fees += FEE;
        n += 2;
    }
    s
}

/// `n` one-input transactions, the first spending a cache coin and each other the one before it.
fn chained(shape: Shape, signed: bool, n: u32) -> Spends {
    let mut s = Spends::default();
    let first = funding(shape, 0);
    s.seeds.push((first, output(COIN, Lock::P2pkh(0))));
    let (mut prev, mut value) = ((first, Lock::P2pkh(0)), COIN);
    for i in 0..n {
        value -= FEE;
        let lock = Lock::P2pkh((i as usize + 1) % 4);
        let tx = spend(&[prev], vec![output(value, lock)], signed);
        prev = (
            OutPoint {
                hash: tx.txid(),
                index: 0,
            },
            lock,
        );
        s.txs.push(Arc::new(tx));
        s.fees += FEE;
    }
    s
}

/// Room left in a block of `limit` bytes for the header, the coinbase and the next transaction.
fn fits(limit: u64) -> impl Fn(usize, u64) -> bool {
    move |_, bytes| bytes + 2_000 <= limit
}

impl Shape {
    fn spends(self, signed: bool) -> Spends {
        let p2pkh = |n: u32| Lock::P2pkh(n as usize % 4);
        match self {
            Shape::Empty => Spends::default(),
            Shape::P2pkh500 | Shape::BadCbAmount => pairs(self, signed, p2pkh, |txs, _| txs < 250),
            Shape::BadMissing => {
                let mut s = pairs(self, signed, p2pkh, |txs, _| txs < 250);
                s.seeds.pop().expect("the last input's coin");
                s
            }
            Shape::Multisig250 => pairs(self, signed, |_| Lock::Multisig, |txs, _| txs < 125),
            Shape::PreDip1Mb => pairs(self, signed, p2pkh, fits(1_000_000)),
            Shape::Dip1TwoMb => pairs(self, signed, p2pkh, fits(2_000_000)),
            Shape::Chained200 => chained(self, signed, 200),
        }
    }
}

/// `CScriptNum`'s serialization of `n`, as BIP34's coinbase pushes the height.
fn script_num(n: u32) -> Vec<u8> {
    let mut bytes: Vec<u8> = n.to_le_bytes().into_iter().collect();
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    if bytes.last().is_some_and(|b| b & 0x80 != 0) {
        bytes.push(0);
    }
    bytes
}

/// The coinbase of the block at `h`: the height (BIP34) and an extra nonce, paying `reward`
/// half to the miner and half to a masternode, as pre-DIP0003 coinbases did.
fn coinbase(h: u32, reward: Amount) -> Tx {
    let mut script_sig = Vec::new();
    push_data(&mut script_sig, &script_num(h));
    push_data(&mut script_sig, &[0x2a; 8]);
    let masternode = reward / 2;
    Tx::new(Transaction {
        version: 1,
        tx_type: TxType::new(0),
        inputs: vec![TxIn {
            prevout: OutPoint {
                hash: TxHash::from([0; 32]),
                index: u32::MAX,
            },
            script_sig: script_sig.into(),
            sequence: u32::MAX,
        }],
        outputs: vec![
            output(reward - masternode, Lock::P2pkh(8)),
            output(masternode, Lock::P2pkh(9)),
        ],
        lock_time: 0,
        extra_payload: Vec::new(),
    })
}

/// The header time at `h`: 150 s blocks from genesis.
fn time_at(params: &ChainSetup, h: u32) -> u32 {
    dash_consensus::params::genesis_block(params).header.time + 150 * h
}

/// A unique hash below the target of mainnet's genesis `nBits` (0x1e0ffff0): its top three
/// little-endian bytes are zero (dash-chain's bench fixture's scheme).
fn synthetic_hash(k: u32) -> BlockHash {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&u64::from(k).wrapping_mul(0x9e37_79b9_7f4a_7c15).to_le_bytes());
    b[8..12].copy_from_slice(&k.to_le_bytes());
    b[12..28].fill(0x5b);
    b[28] = 0x01;
    BlockHash::from(b)
}

/// `GetBlockSubsidy` of the block at `h`, whose parent is synthetic.
fn subsidy(params: &ChainSetup, h: u32) -> Amount {
    let payments = PaymentsParams::from_chain_params(params).expect("mainnet's payments");
    let prev = HeaderCtx {
        height: h - 1,
        hash: synthetic_hash(h - 1),
        time: time_at(params, h - 1),
        bits: CompactTarget::new(dash_consensus::params::genesis_block(params).header.bits),
        version: VERSION,
        mtp: 0,
        chain_work: Work::ZERO,
    };
    block_subsidy(Some(&prev), false, payments.subsidy()).expect("a subsidy")
}

/// `raw` with the first nonce from `hint` on whose X11 hash meets its `nBits`.
fn mine(params: &ChainSetup, mut raw: BlockHeader, hint: u32, shape: Shape, av: Av) -> Header {
    let hp = HeaderParams::from_chain_params(params).expect("mainnet's header parameters");
    let bits = CompactTarget::new(raw.bits);
    raw.nonce = hint;
    loop {
        let header = Header::new(raw);
        if check_proof_of_work(header.hash().as_bytes(), bits, hp.pow()) {
            if raw.nonce != hint {
                eprintln!(
                    "connect_block_micro: {}/{} mines at nonce {}, not NONCES' {hint}",
                    shape.name(),
                    av.name(),
                    raw.nonce
                );
            }
            return header;
        }
        raw.nonce = raw.nonce.checked_add(1).expect("a nonce below 2^32");
    }
}

/// The shape's block for `av` on its synthetic parent, and the coins it spends from the cache.
fn build(params: &ChainSetup, shape: Shape, av: Av) -> (Block, Vec<(OutPoint, TxOut)>) {
    let h = shape.height();
    let s = shape.spends(av.signed());
    let over = Amount::from(matches!(shape, Shape::BadCbAmount));
    let mut txs = vec![Arc::new(coinbase(h, subsidy(params, h) + s.fees + over))];
    txs.extend(s.txs);
    let merkle_root = Block::new(Header::new(BlockHeader::default()), txs.clone()).merkle().0;
    let raw = BlockHeader {
        version: VERSION,
        prev_hash: synthetic_hash(h - 1),
        merkle_root,
        time: time_at(params, h),
        bits: dash_consensus::params::genesis_block(params).header.bits,
        nonce: 0,
    };
    let header = mine(params, raw, NONCES[shape as usize][av as usize], shape, av);
    let block = Block::new(header, txs);
    let limit = match shape {
        Shape::Dip1TwoMb => 2_000_000,
        _ => 1_000_000,
    };
    assert!(u64::from(block.size()) <= limit, "{} is {} bytes", shape.name(), block.size());
    (block, s.seeds)
}

// ---- the index -------------------------------------------------------------------------------

struct NoFail;

impl FailStop for NoFail {
    fn read_failed(&self, what: &str) {
        panic!("bench store read failed: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

/// A node's index of mainnet's genesis, synthetic headers to the block's parent (each connected,
/// with data and undo), the block (accepted: `BLOCK_VALID_TRANSACTIONS` with data) and
/// [`AV_LEAD`] headers after it, under `-minimumchainwork=0`.
fn index(params: &Arc<ChainSetup>, pool: &rayon::ThreadPool, block: &Block, h: u32) -> HeaderChain {
    let svc = Services {
        clock: Arc::new(MockClock::at(1_700_000_000)),
        sporks: Arc::new(StaticSporks::for_network(params, None)),
        fail: Arc::new(NoFail),
    };
    let cfg = || ChainConfig {
        min_chain_work: Some(Work::ZERO),
        ..ChainConfig::node(params)
    };
    let mut genesis: HeaderChain =
        HeaderChain::new_genesis(params.clone(), cfg(), svc.clone(), NoStore).expect("genesis");
    let g = genesis.node_index().expect("a node").take_dirty().pop().expect("the genesis record");
    let (bip34_height, bip34_hash) = params.consensus.bip34;
    let genesis_bits = dash_consensus::params::genesis_block(params).header.bits;
    let bip34 = (bip34_height.to_u32(), BlockHash::from(*bip34_hash.as_bytes()));
    let hash_at = |k: u32| match k {
        0 => g.hash,
        k if k == h => block.hash(),
        k if k == bip34.0 => bip34.1,
        k => synthetic_hash(k),
    };
    let synthetic = |k: u32| BlockHeader {
        version: VERSION,
        prev_hash: hash_at(k - 1),
        time: time_at(params, k),
        bits: genesis_bits,
        nonce: k,
        ..BlockHeader::default()
    };
    let records = (1..=h + AV_LEAD).map(|k| {
        let record = match k.cmp(&h) {
            std::cmp::Ordering::Less => BlockIndexRecord {
                height: k,
                status: Status(Status::VALID_SCRIPTS | Status::HAVE_DATA | Status::HAVE_UNDO),
                n_tx: 1 + k % 64,
                file: Some((k / 1500) as i32),
                data_pos: Some(8 + (k % 1500) * 4096),
                undo_pos: Some(8 + (k % 1500) * 512),
                header: synthetic(k),
            },
            std::cmp::Ordering::Equal => BlockIndexRecord {
                height: k,
                status: accepted(),
                n_tx: block.txs().len() as u32,
                file: Some(0),
                data_pos: Some(8),
                undo_pos: None,
                header: *block.header().raw(),
            },
            std::cmp::Ordering::Greater => BlockIndexRecord {
                height: k,
                status: Status(Status::VALID_TREE),
                n_tx: 0,
                file: None,
                data_pos: None,
                undo_pos: None,
                header: synthetic(k),
            },
        };
        Ok::<_, LoadError>((hash_at(k), record.to_bytes()))
    });
    let records = std::iter::once(Ok((g.hash, g.bytes))).chain(records);
    HeaderChain::load(params.clone(), cfg(), svc, records, pool).expect("the index loads")
}

/// The block's status as `AcceptBlock` leaves it, before `ConnectBlock`.
fn accepted() -> Status {
    Status(Status::VALID_TRANSACTIONS | Status::HAVE_DATA)
}

// ---- the connect -----------------------------------------------------------------------------

/// No ChainLock below DIP0008, as on mainnet at these heights.
pub(crate) struct NoChainLocks;

impl ChainLockView for NoChainLocks {
    fn has_chainlock(&self, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn has_conflicting_chainlock(&self, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn best_height(&self) -> i32 {
        -1
    }
}

/// The undo files: `blocks/` in a private temporary directory, mainnet's magic.
pub(crate) struct Undo {
    dir: TempDir,
    fs: Arc<OsFs>,
    magic: [u8; 4],
    latch: GlobalFailStop,
    pub(crate) files: RefCell<GuardedFiles<OsFs>>,
}

impl Undo {
    fn new(params: &ChainSetup, latch: &GlobalFailStop) -> Undo {
        let dir = tempfile::Builder::new().prefix("connect_block_micro").tempdir().expect("a dir");
        std::fs::DirBuilder::new().mode(0o700).create(dir.path().join("blocks")).expect("blocks/");
        let fs = Arc::new(OsFs::new());
        let files = RefCell::new(Undo::open(&fs, &dir, params.message_start, latch));
        Undo {
            dir,
            fs,
            magic: params.message_start,
            latch: latch.clone(),
            files,
        }
    }

    fn open(
        fs: &Arc<OsFs>,
        dir: &TempDir,
        magic: [u8; 4],
        latch: &GlobalFailStop,
    ) -> GuardedFiles<OsFs> {
        let cfg = BlockFilesConfig::new(dir.path().join("blocks"), magic);
        let files = BlockFiles::with_fs(fs.clone(), cfg, BlockFilesState::default());
        GuardedFiles::new(files, latch.clone())
    }

    fn rev(&self) -> PathBuf {
        self.dir.path().join("blocks").join("rev00000.dat")
    }

    /// A rev file whose records pass [`ROTATE`] starts over, empty, as a node's next rev file
    /// would. Its records' size, not its length: ST2's allocation only grows the file.
    fn rotate(&self) {
        let size = self.files.borrow().read(|f| f.file_info(0).map_or(0, |i| i.undo_size));
        if u64::from(size) > ROTATE {
            std::fs::remove_file(self.rev()).expect("the rev file");
            *self.files.borrow_mut() = Undo::open(&self.fs, &self.dir, self.magic, &self.latch);
        }
    }
}

/// Outside the region: the block's entry back to accepted, the index's dirty set drained, the rev
/// file rotated.
pub(crate) fn reset(chain: &RefCell<&mut HeaderChain>, id: BlockId, undo: &Undo) {
    let mut chain = chain.borrow_mut();
    let mut ix = chain.node_index().expect("a node index");
    ix.modify(id, |g| g.status = accepted());
    drop(ix.take_dirty());
    undo.rotate();
}

/// One `ConnectBlock` of `block` (entry `id`) into an overlay over `coins`, dropped uncommitted.
#[expect(clippy::too_many_arguments, reason = "SerialCore::connect's arguments and their stores")]
fn connect<'c>(
    core: &mut SerialCore,
    steps: &mut NoEvoSteps,
    cx: &ConnectCx<'_>,
    coins: &mut CoinsCache<MemoryEngine>,
    undo: &Undo,
    block: &Block,
    id: BlockId,
    chain: &'c RefCell<&'c mut HeaderChain>,
) -> ConnectOutcome {
    let mut overlay = BlockOverlay::new(coins);
    let mut st = status_writer(chain, id);
    let files = undo.files.borrow();
    core.connect(steps, cx, &mut overlay, &files, block, true, id, &mut st)
        .expect("no storage fault")
}

/// One row's world: its block's index, the warm cache holding the coins it spends, its undo files,
/// and a core at its `-assumevalid` setting.
pub(crate) struct Fixture {
    pub(crate) params: Arc<ChainSetup>,
    pub(crate) chain: HeaderChain,
    pub(crate) reader: ChainReader,
    pub(crate) ancestry: CompleteAncestry,
    pub(crate) versionbits: VersionBitsCache,
    pub(crate) coins: CoinsCache<MemoryEngine>,
    pub(crate) undo: Undo,
    pub(crate) block: Block,
    pub(crate) id: BlockId,
    pub(crate) core: SerialCore,
    /// Whether the unmeasured first connect has run (criterion calls a row's routine repeatedly).
    pub(crate) warm: bool,
    pub(crate) steps: NoEvoSteps,
    pub(crate) abort: Arc<RecordingAbort>,
    pub(crate) expect: Expect,
}

impl Fixture {
    pub(crate) fn new(
        params: &Arc<ChainSetup>,
        pool: &rayon::ThreadPool,
        par: Par,
        shape: Shape,
        av: Av,
    ) -> Fixture {
        let (block, seeds) = build(params, shape, av);
        let h = shape.height();
        let chain = index(params, pool, &block, h);
        let reader = chain.reader();
        let id = reader.lookup(&block.hash()).expect("the block is indexed");
        let prev = BlockRef {
            id: reader.lookup(&synthetic_hash(h - 1)).expect("its parent is indexed"),
            hash: synthetic_hash(h - 1),
            height: h - 1,
        };
        let best = reader.entry(chain.best_header()).expect("the best header");
        assert_eq!(best.height, h + AV_LEAD);
        assert_eq!(reader.ancestor(chain.best_header(), h), Some(id), "the block leads to it");

        let latch = GlobalFailStop::new();
        let db = Arc::new(GuardedEngine::new(
            MemoryEngine::new(&CHAINSTATE_SCHEMA),
            latch.clone(),
            Arc::new(RecordingSink::default()),
        ));
        let mut coins = CoinsCache::new(
            db,
            CoinsBudget::new(64 * MIB, 0, Ratio::HALF, 0),
            Salt::fixed(7),
            None,
        );
        for (op, out) in &seeds {
            let src = CoinSrc::Output {
                out,
                height: h - COIN_AGE,
                coinbase: false,
            };
            coins.add(&OutPointKey::new(op), src, false).expect("seeded");
        }
        coins.set_best_block(Some(prev));

        // Core's default cache sizes, salted with fixed nonces so every run probes the same
        // buckets.
        let sizes = CacheSizes::default();
        let caches = Arc::new(ScriptCaches {
            sig: SigCache::with_nonce(sizes.signature_cache_bytes, [0x5a; 32]).expect("sigcache"),
            exec: ScriptExecCache::with_nonce(sizes.script_execution_cache_bytes, [0xa5; 32])
                .expect("script execution cache"),
        });
        let abort = Arc::new(RecordingAbort::default());
        let path = AbortPath::new(abort.clone(), latch.clone());
        let assume_valid = match av {
            Av::Off => None,
            Av::On => Some(best.hash),
        };
        let queue = Arc::new(par.queue());
        let core = SerialCore::new(params, caches, queue, Box::new(NodeHelper), path, assume_valid)
            .expect("mainnet's core");
        Fixture {
            params: params.clone(),
            undo: Undo::new(params, &latch),
            ancestry: chain.complete_ancestry().expect("a node chain"),
            chain,
            reader,
            versionbits: VersionBitsCache::new(),
            coins,
            block,
            id,
            core,
            warm: false,
            steps: NoEvoSteps::default(),
            abort,
            expect: shape.expect(),
        }
    }

    /// `measure`s the connect with its per-iteration setup, after one unmeasured connect that
    /// checks the outcome and what it wrote.
    fn run(&mut self, measure: impl FnOnce(&mut dyn FnMut(), &mut dyn FnMut() -> ConnectOutcome)) {
        let Fixture {
            params,
            chain,
            reader,
            ancestry,
            versionbits,
            coins,
            undo,
            block,
            id,
            core,
            warm,
            steps,
            abort,
            expect,
        } = self;
        let (id, expect, block, undo) = (*id, *expect, &*block, &*undo);
        let deployments = Deployments::new(params, ancestry, &NoEhfSignals, versionbits);
        let cx = ConnectCx {
            params,
            reader,
            deployments: &deployments,
            best_header: chain.best_header(),
            min_chain_work: reader.min_chain_work(),
            is_ibd: true,
            chainlocks: &NoChainLocks,
            guard: GuardScope::Live,
            origin: AttemptOrigin::Tip,
        };
        let chain = RefCell::new(chain);
        if !*warm {
            reset(&chain, id, undo);
            let out = connect(core, steps, &cx, coins, undo, block, id, &chain);
            expect.check(&out);
            assert!(abort.messages().is_empty(), "{:?}", abort.messages());
            assert!(steps.effects().is_empty(), "no effect below DIP0003");
            let status = reader.entry(id).expect("indexed").status;
            let connected = matches!(expect, Expect::Connected);
            assert_eq!(status.has(Status::HAVE_UNDO), connected, "{status:?}");
            assert_eq!(status.is_valid(Status::VALID_SCRIPTS), connected, "{status:?}");
            let rev = std::fs::metadata(undo.rev()).map_or(0, |m| m.len());
            assert_eq!(rev > 0, connected, "the undo record's write");
            *warm = true;
        }
        let mut setup = || reset(&chain, id, undo);
        let mut routine = || {
            let out = connect(core, steps, &cx, coins, undo, block, id, &chain);
            expect.check(&out);
            out
        };
        measure(&mut setup, &mut routine);
    }
}

fn connect_block_micro(c: &mut Criterion) {
    let params = Arc::new(
        ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("Core's parameters"),
    );
    // One per process: it runs on the bench thread (`use_current_thread`), so with
    // RAYON_NUM_THREADS=1 no worker exists to spin into a measured region (dash-chain's fixture).
    // A par4 row's queue has its own three threads, whatever RAYON_NUM_THREADS says.
    let pool = rayon::ThreadPoolBuilder::new().use_current_thread().build().expect("a rayon pool");
    for par in [Par::Serial, Par::Par4] {
        for shape in Shape::ALL {
            for av in [Av::Off, Av::On] {
                // Built on the row's first call (only a selected row's), and dropped after it.
                let mut fixture = None;
                let id = format!("{}/{}/{}", par.name(), shape.name(), av.name());
                c.bench_function(&id, |b| {
                    let fx =
                        fixture.get_or_insert_with(|| Fixture::new(&params, &pool, par, shape, av));
                    fx.run(|setup, routine| {
                        b.iter_batched(setup, |()| routine(), BatchSize::PerIteration);
                    });
                });
            }
        }
    }
}

criterion_group!(benches, connect_block_micro);
criterion_main!(benches);

//! `diff_cmpct_initdata` (P3 §6.4, E4): Core's `PartiallyDownloadedBlock::InitData` and
//! `FillBlock` (`oracle_partially_downloaded_block`, ABI 34) against `dashd-p2p`'s
//! [`PartiallyDownloadedBlock`] on the same `cmpctblock`, mempool, extra-transaction list and
//! `blocktxn`. This is where the libstdc++ bucket rule (P3 D7, D22) meets Core's real
//! `std::unordered_map`.
//!
//! Compared: the `InitData` status, the `FillBlock` status (none when `InitData` was not `Ok`)
//! and the reconstructed block's bytes; and, where the inputs are raw, which argument each side
//! refuses to decode first, in Core's order (`cmpctblock`, mempool, extra list, `blocktxn`). Core
//! stops at the first argument it cannot decode, and its error does not always name it, so on a
//! refusal Core's argument is found by calling it again with each prefix of the arguments. The
//! node side decodes the very bytes Core is given, through the handler's reads
//! (`policy::read` for `cmpctblock` and `blocktxn`), its mempool walks the transactions in the
//! order Core's `vTxHashes` holds them, and its extra ring holds them in Core's slot order.
//!
//! The input's first byte picks the form (mod 3):
//!
//! - **0, built.** A block of synthetic transactions, as `cmpct_reconstruct` builds it (a
//!   coinbase, then spends; 1–64 of them; the coinbase may be a spend instead, and the spends
//!   may be 64 bytes long), or the fixture block whose two decoy transactions share a short ID
//!   ([`FIXTURE_PAIR`]). Decoys are transactions outside the block. Each block transaction is
//!   prefilled, in the mempool, in the extra list, or none of these, and its short ID may be a
//!   decoy's instead of its own; each decoy may be in the mempool and the extra list. A key byte
//!   per entry orders the mempool walk and another the extra list. Then one perturbation of the
//!   `blocktxn`
//!   that answers the slots the node left missing: none, drop one, add one, swap two, replace
//!   one, or a header whose merkle root is wrong.
//! - **1, transition.** P3 D22's sets: a size from [`transition_sizes`] (n−1, n and n+1 at every
//!   change of the bucket count), its short IDs made with 12 or 13 of them in one bucket (first,
//!   last or interleaved) and one in each of the others. The coinbase may be prefilled; a block
//!   of up to [`FILL_MAX`] transactions may answer the slots.
//! - **2, raw.** A `cmpctblock` payload, mempool and extra transactions, and a `blocktxn` payload
//!   or none, as the input gives them (`u16`-length-prefixed chunks).
//!
//! Core refuses a mempool with two transactions of one txid (`ErrorKind::Argument`), so both
//! sides get the mempool with each txid's first entry only. The target never feeds duplicates,
//! and Core's refusal after that is a divergence of the txids.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock};

use dash_consensus::primitives::{core_serialize, write_compact_size};
use dash_consensus::{Block, Header, Tx, TxRef, decode_tx};
use dash_oracle_sys::{Chain, CompactFill, ErrorKind, Oracle, Pin, ReadStatus};
use dash_p2p::policy;
use dash_p2p_core::{BlockTxn, CmpctBlock, ProtoCtx, ProtocolVersion};
use dash_pkc::bls::BlsCtx;
use dash_primitives::{BlockHash, BlockHeader, MerkleRoot};
use dash_types::Numeric as _;
use dashd_p2p::processing::compact::{
    self as port, CompactBlock, CompactTxSource, ExtraTxns, PartiallyDownloadedBlock, Prefilled,
    bucket_count,
};

use crate::{Compared, Divergence, Outcome, check, divergence};

/// The context the handler reads `cmpctblock` and `blocktxn` under at v24 (neither reads the
/// version or the BLS scheme).
const CTX: ProtoCtx = ProtoCtx::new(ProtocolVersion::new(70242), BlsCtx::BASIC);

/// The largest transition set whose block a `blocktxn` answers.
pub const FILL_MAX: usize = 2048;

/// The fixture block's coinbase and spend seeds ([`coinbase`], [`spend`]).
pub const FIXTURE_SEEDS: (u32, [u32; 2]) = (1, [2, 3]);

/// The fixture `cmpctblock`'s nonce.
pub const FIXTURE_NONCE: u64 = 7;

/// Seeds of two [`spend`]s, outside the fixture block, whose instance hashes share one short ID
/// under the fixture `cmpctblock`'s keys.
pub const FIXTURE_PAIR: (u32, u32) = (1_598_304, 15_891_244);

/// Reads the input front to back; past its end every read is 0.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn byte(&mut self) -> u8 {
        let (&b, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        b
    }

    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        std::array::from_fn(|_| self.byte())
    }

    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.bytes())
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.bytes())
    }

    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.bytes())
    }

    /// A `u16`-length-prefixed slice, cut short at the input's end.
    fn chunk(&mut self) -> Vec<u8> {
        let len = usize::from(self.u16());
        let (chunk, rest) = self.0.split_at(len.min(self.0.len()));
        self.0 = rest;
        chunk.to_vec()
    }
}

/// A version 1 transaction's Core serialization: one input spending `prevout` with
/// `script_sig`, one output of `value` paying `OP_TRUE`, lock time 0. It is 61 bytes long plus
/// the `script_sig`'s length.
fn raw_tx(prevout: ([u8; 32], u32), script_sig: &[u8], value: u64) -> Vec<u8> {
    let mut raw = vec![1, 0, 0, 0, 1];
    raw.extend_from_slice(&prevout.0);
    raw.extend_from_slice(&prevout.1.to_le_bytes());
    raw.push(script_sig.len() as u8);
    raw.extend_from_slice(script_sig);
    raw.extend_from_slice(&[0xff; 4]);
    raw.push(1);
    raw.extend_from_slice(&value.to_le_bytes());
    raw.extend_from_slice(&[1, 0x51]);
    raw.extend_from_slice(&[0; 4]);
    raw
}

fn decoded(raw: &[u8]) -> Option<TxRef> {
    decode_tx(&mut &raw[..]).ok().map(|d| Arc::new(d.into_value()))
}

/// The coinbase of `seed`'s Core serialization.
pub fn raw_coinbase(seed: u32) -> Vec<u8> {
    let mut script = seed.to_le_bytes().to_vec();
    script.push(0x51);
    raw_tx(([0; 32], u32::MAX), &script, 50)
}

/// The spend of `seed`'s Core serialization: it spends output 0 of a txid that starts with
/// `seed`.
pub fn raw_spend(seed: u32) -> Vec<u8> {
    let mut prevout = [0; 32];
    prevout[..4].copy_from_slice(&seed.to_le_bytes());
    raw_tx((prevout, 0), &[], 42)
}

/// The spend of `seed` with a 3-byte `script_sig`: 64 bytes long, which `IsBlockMutated` reads
/// in a block whose first transaction is not a coinbase.
pub fn raw_spend64(seed: u32) -> Vec<u8> {
    let mut prevout = [0; 32];
    prevout[..4].copy_from_slice(&seed.to_le_bytes());
    raw_tx((prevout, 0), &[0x51; 3], 42)
}

/// The coinbase of `seed` (as `cmpct_reconstruct` makes it).
pub fn coinbase(seed: u32) -> Option<TxRef> {
    decoded(&raw_coinbase(seed))
}

/// The spend of `seed` (as `cmpct_reconstruct` makes it).
pub fn spend(seed: u32) -> Option<TxRef> {
    decoded(&raw_spend(seed))
}

/// The 64-byte spend of `seed` ([`raw_spend64`]).
pub fn spend64(seed: u32) -> Option<TxRef> {
    decoded(&raw_spend64(seed))
}

fn header(merkle_root: MerkleRoot, nonce: u32) -> Header {
    Header::new(BlockHeader::new(
        0x2000_0000,
        BlockHash::ZERO,
        merkle_root,
        1_700_000_000,
        0x207f_ffff,
        nonce,
    ))
}

/// A block over `txs` whose header commits to their merkle root, or to a wrong one.
fn block(txs: Vec<TxRef>, header_nonce: u32, wrong_root: bool) -> Block {
    let root = if wrong_root {
        MerkleRoot::from_lendian([0xab; 32])
    } else {
        Block::new(header(MerkleRoot::ZERO, 0), txs.clone()).merkle().0
    };
    Block::new(header(root, header_nonce), txs)
}

/// The fixture block: a coinbase and two spends ([`FIXTURE_SEEDS`]).
pub fn fixture_block() -> Option<Block> {
    let (cb, spends) = FIXTURE_SEEDS;
    let mut txs = vec![coinbase(cb)?];
    for seed in spends {
        txs.push(spend(seed)?);
    }
    Some(block(txs, 0, false))
}

/// The fixture `cmpctblock` (its coinbase prefilled), whose keys [`FIXTURE_PAIR`] collides
/// under.
pub fn fixture_compact() -> Option<CompactBlock> {
    Some(CompactBlock::from_block(&fixture_block()?, FIXTURE_NONCE))
}

/// Every short-ID count of D22's transition sets: n−1, n and n+1 wherever the bucket count of
/// `std::unordered_map<uint64_t, uint16_t>(n)` changes, within 0..=65,535.
pub fn transition_sizes() -> &'static [usize] {
    static SIZES: OnceLock<Vec<usize>> = OnceLock::new();
    SIZES.get_or_init(|| {
        let mut sizes: Vec<usize> = (1..=65_535usize)
            .filter(|&n| bucket_count(n) != bucket_count(n - 1))
            .flat_map(|n| [n - 1, n, n + 1])
            .filter(|&n| n <= 65_535)
            .collect();
        sizes.sort_unstable();
        sizes.dedup();
        sizes
    })
}

/// How the `blocktxn` is made.
enum Answer {
    /// As the input gives it (None: no `blocktxn`).
    Raw(Option<Vec<u8>>),
    /// The block's own transactions in the slots the node's `InitData` left missing, perturbed;
    /// no `blocktxn` when that `InitData` is not `Ok`.
    Missing {
        block: Block,
        perturb: u8,
        pick: usize,
    },
}

/// One comparison's inputs, as Core is given them (the mempool before its txid filter).
struct Case {
    cmpct: Vec<u8>,
    pool: Vec<Vec<u8>>,
    extra: Vec<Vec<u8>>,
    answer: Answer,
}

/// What both sides agreed on, for the tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agreed {
    pub outcome: Outcome,
    /// Core's result; None when both refused the inputs' decoding.
    pub fill: Option<CompactFill>,
    /// The argument both refused first; None when both decoded every argument.
    pub refused: Option<Arg>,
}

/// The arguments, in the order Core decodes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    Cmpct,
    Pool,
    Extra,
    Blocktxn,
}

/// `diff_cmpct_initdata`.
pub fn compare_cmpct_initdata(bytes: &[u8], pin: Pin) -> Compared {
    compare_with(&Oracle::new(pin), bytes).map(|a| a.outcome)
}

/// The target's start: the transition sizes, and one comparison, so that the entry point's
/// one-time set-up is in no input's coverage.
pub fn start(pin: Pin) {
    let _ = transition_sizes();
    let _ = compare_with(&Oracle::new(pin), &[0]);
}

/// The pins whose entry point has built its mock mempool's dependencies in this process.
static READY: Mutex<Vec<Pin>> = Mutex::new(Vec::new());

/// Selects Core's mainnet parameters until the entry point has run once. Its first mempool
/// builds a static `PoolDeps` whose `CSporkManager` reads the data directory, so it asserts on
/// `BaseParams()` in a process with no chain selected (a session's teardown also clears it);
/// nothing the entry point computes depends on the chain.
fn select_chain(oracle: &Oracle) {
    let ready = READY.lock().unwrap_or_else(|e| e.into_inner());
    if !ready.contains(&oracle.pin()) && oracle.chainparams_json(&Chain::main()).is_err() {
        panic!("diff_cmpct_initdata: Core's mainnet parameters could not be selected");
    }
}

fn mark_ready(pin: Pin) {
    let mut ready = READY.lock().unwrap_or_else(|e| e.into_inner());
    if !ready.contains(&pin) {
        ready.push(pin);
    }
}

/// The comparison of `bytes` on `oracle`, with Core's result.
pub fn compare_with(oracle: &Oracle, bytes: &[u8]) -> Result<Agreed, Divergence> {
    select_chain(oracle);
    let Some(case) = case_from_bytes(bytes) else {
        return Ok(Agreed {
            outcome: Outcome::OutOfDomain,
            fill: None,
            refused: None,
        });
    };
    compare_case(oracle, case)
}

/// The inputs `bytes` stand for; None only if the generator's own transactions fail to decode.
fn case_from_bytes(bytes: &[u8]) -> Option<Case> {
    let mut input = Input(bytes);
    match input.byte() % 3 {
        0 => built(input),
        1 => transition(input),
        _ => Some(raw(input)),
    }
}

fn wire(tx: &TxRef) -> Vec<u8> {
    core_serialize(&**tx)
}

/// Rewrites `cmpct`'s prefilled transactions and short IDs: slot `i` is prefilled when
/// `prefilled(i)`, else carries `short_id(i)`.
fn lay_out(
    cmpct: &mut CompactBlock,
    txs: &[TxRef],
    prefilled: impl Fn(usize) -> bool,
    short_id: impl Fn(&CompactBlock, usize) -> u64,
) {
    cmpct.prefilled.clear();
    let mut next = 0;
    for i in (0..txs.len()).filter(|&i| prefilled(i)) {
        cmpct.prefilled.push(Prefilled {
            index: (i - next) as u16,
            tx: Arc::clone(&txs[i]),
        });
        next = i + 1;
    }
    let ids = (0..txs.len()).filter(|&i| !prefilled(i)).map(|i| short_id(cmpct, i)).collect();
    cmpct.short_ids = ids;
}

fn built(mut input: Input<'_>) -> Option<Case> {
    let fixture = input.byte() & 1 == 1;
    let (txs, decoys, nonce) = if fixture {
        let txs = fixture_block()?.txs().to_vec();
        (txs, vec![spend(FIXTURE_PAIR.0)?, spend(FIXTURE_PAIR.1)?], FIXTURE_NONCE)
    } else {
        let n = 1 + usize::from(input.byte() % 64);
        let shape = input.byte();
        let spend_of = |seed| {
            if shape & 2 != 0 {
                spend64(seed)
            } else {
                spend(seed)
            }
        };
        let first = input.u32();
        let mut txs = vec![if shape & 1 != 0 {
            spend_of(first)?
        } else {
            coinbase(first)?
        }];
        for _ in 1..n {
            txs.push(spend_of(input.u32())?);
        }
        let mut decoys = Vec::new();
        for _ in 0..input.byte() % 4 {
            decoys.push(spend(input.u32())?);
        }
        (txs, decoys, input.u64())
    };
    // Per entry: its flags, its mempool key and its extra-list key.
    let mut entry = || (input.byte(), input.byte(), input.byte());
    let tx_flags: Vec<(u8, u8, u8)> = txs.iter().map(|_| entry()).collect();
    let decoy_flags: Vec<(u8, u8, u8)> = decoys.iter().map(|_| entry()).collect();
    let perturb = input.byte() % 6;
    let pick = usize::from(input.byte());

    let header_nonce = if fixture {
        0
    } else {
        pick as u32
    };
    let block = block(txs.clone(), header_nonce, perturb == 5);
    let mut cmpct = CompactBlock::from_block(&block, nonce);
    let decoy_of = |flags: u8| {
        (flags & 8 != 0 && !decoys.is_empty())
            .then(|| &decoys[usize::from(flags >> 4) % decoys.len()])
    };
    lay_out(
        &mut cmpct,
        &txs,
        |i| tx_flags[i].0 & 1 != 0,
        |c, i| c.short_id(&decoy_of(tx_flags[i].0).unwrap_or(&txs[i]).instance_hash()),
    );

    let entries: Vec<(&TxRef, (u8, u8, u8))> =
        txs.iter().zip(tx_flags).chain(decoys.iter().zip(decoy_flags)).collect();
    // The entries with `bit` in their flags, ordered by `key` (stable).
    let ordered = |bit: u8, key: fn((u8, u8, u8)) -> u8| {
        let mut list: Vec<(u8, &TxRef)> =
            entries.iter().filter(|(_, e)| e.0 & bit != 0).map(|&(tx, e)| (key(e), tx)).collect();
        list.sort_by_key(|&(k, _)| k);
        list.into_iter().map(|(_, tx)| wire(tx)).collect::<Vec<_>>()
    };
    Some(Case {
        cmpct: cmpct.wire_bytes(),
        pool: ordered(2, |e| e.1),
        extra: ordered(4, |e| e.2),
        answer: Answer::Missing {
            block,
            perturb,
            pick,
        },
    })
}

fn transition(mut input: Input<'_>) -> Option<Case> {
    let sizes = transition_sizes();
    let n = sizes[usize::from(input.u16()) % sizes.len()];
    let flags = input.byte();
    let crowd = (12 + usize::from(flags & 1)).min(n);
    let buckets = bucket_count(n);
    let home = u64::from(input.u32()) % buckets;
    let high = u64::from(input.u16());
    // The crowd in bucket `home`, and one ID in each of the next n − crowd buckets (n ≤ buckets).
    let mut crowded = (0..crowd as u64).map(|k| home + buckets * (high + k));
    let mut others = (1..=(n - crowd) as u64).map(|j| (home + j) % buckets + buckets * high);
    let ids: Vec<u64> = match (flags >> 1) % 3 {
        0 => crowded.chain(others).collect(),
        1 => others.chain(crowded).collect(),
        _ => (0..n)
            .filter_map(|i| {
                if i % 2 == 0 {
                    crowded.next().or_else(|| others.next())
                } else {
                    others.next().or_else(|| crowded.next())
                }
            })
            .collect(),
    };
    let prefill_coinbase = flags & 8 != 0;
    let fill = flags & 16 != 0 && n <= FILL_MAX;
    let slots = n + usize::from(prefill_coinbase);
    let mut txs = vec![coinbase(n as u32)?];
    if fill {
        for seed in 1..slots as u32 {
            txs.push(spend(seed)?);
        }
    }
    let block = block(txs, 0, false);
    let mut cmpct = CompactBlock::from_block(&block, 0);
    cmpct.prefilled.retain(|_| prefill_coinbase);
    cmpct.short_ids = ids;
    Some(Case {
        cmpct: cmpct.wire_bytes(),
        pool: Vec::new(),
        extra: Vec::new(),
        answer: if fill {
            Answer::Missing {
                block,
                perturb: 0,
                pick: 0,
            }
        } else {
            Answer::Raw(None)
        },
    })
}

fn raw(mut input: Input<'_>) -> Case {
    let cmpct = input.chunk();
    let pool = (0..input.byte() % 8).map(|_| input.chunk()).collect();
    let extra = (0..input.byte() % 8).map(|_| input.chunk()).collect();
    let blocktxn = (input.byte() & 1 == 1).then(|| input.chunk());
    Case {
        cmpct,
        pool,
        extra,
        answer: Answer::Raw(blocktxn),
    }
}

/// The mempool in `vTxHashes` order.
struct Pool(Vec<TxRef>);

impl CompactTxSource for Pool {
    fn for_each_in_vtxhashes_order(
        &self,
        f: &mut dyn FnMut(&dash_primitives::TxHash, &TxRef) -> bool,
    ) {
        for tx in &self.0 {
            if !f(&tx.instance_hash(), tx) {
                return;
            }
        }
    }
}

/// `cmpctblock` as the handler reads it, whole.
fn read_cmpct(bytes: &[u8]) -> Option<CompactBlock> {
    let mut p = bytes;
    let msg = policy::read::<CmpctBlock>(&mut p, &CTX).ok()?;
    p.is_empty().then(|| CompactBlock::from_msg(msg))
}

/// A `blocktxn` as the handler reads it, whole; `Some(empty)` for none or an empty payload (Core
/// runs `FillBlock` with no transactions then).
fn read_blocktxn(bytes: Option<&[u8]>) -> Option<Vec<TxRef>> {
    let Some(mut p) = bytes.filter(|b| !b.is_empty()) else {
        return Some(Vec::new());
    };
    let msg = policy::read::<BlockTxn>(&mut p, &CTX).ok()?;
    p.is_empty().then(|| msg.txs().iter().cloned().map(|tx| Arc::new(Tx::new(tx))).collect())
}

/// The transactions as Core reads the list it builds from them (a count, then each one's bytes),
/// with each one's own bytes.
fn read_txs(txs: &[Vec<u8>]) -> Option<Vec<(Vec<u8>, TxRef)>> {
    let list = txs.concat();
    let mut cursor = &list[..];
    let mut out = Vec::with_capacity(txs.len());
    for _ in txs {
        let d = decode_tx(&mut cursor).ok()?;
        out.push((d.wire().to_vec(), Arc::new(d.into_value())));
    }
    cursor.is_empty().then_some(out)
}

fn status(s: port::ReadStatus) -> ReadStatus {
    match s {
        port::ReadStatus::Ok => ReadStatus::Ok,
        port::ReadStatus::Invalid => ReadStatus::Invalid,
        port::ReadStatus::Failed => ReadStatus::Failed,
    }
}

/// The `blocktxn` payload of `txs` for `hash`, in Core's serialization.
fn blocktxn_payload(hash: BlockHash, txs: &[TxRef]) -> Vec<u8> {
    let mut out = hash.as_bytes().to_vec();
    write_compact_size(txs.len() as u64, &mut out);
    for tx in txs {
        out.extend(wire(tx));
    }
    out
}

/// The block's transactions in `partial`'s missing slots, perturbed as `cmpct_reconstruct`
/// perturbs them.
fn answer(
    partial: &PartiallyDownloadedBlock,
    block: &Block,
    perturb: u8,
    pick: usize,
) -> Option<Vec<TxRef>> {
    let extra_seed = u32::MAX - pick as u32;
    let mut txn: Vec<TxRef> = (0..partial.tx_count())
        .filter(|&i| !partial.is_tx_available(i))
        .filter_map(|i| block.txs().get(i).cloned())
        .collect();
    match perturb {
        1 if !txn.is_empty() => {
            txn.remove(pick % txn.len());
        }
        2 => txn.insert(pick % (txn.len() + 1), spend(extra_seed)?),
        3 if txn.len() > 1 => {
            let (i, len) = (pick % txn.len(), txn.len());
            txn.swap(i, (i + 1) % len);
        }
        4 if !txn.is_empty() => {
            let i = pick % txn.len();
            txn[i] = spend(extra_seed)?;
        }
        _ => {}
    }
    Some(txn)
}

/// The `blocktxn` the node and Core are given, and the node's `InitData` and `FillBlock` on the
/// decoded inputs (None when it cannot decode the `blocktxn`).
fn node_side(
    cmpct: &CompactBlock,
    pool: &[TxRef],
    extra: &[TxRef],
    answer_with: &Answer,
) -> (Option<CompactFill>, Option<Vec<u8>>) {
    let mut ring = ExtraTxns::new(extra.len() as i64);
    for tx in extra {
        ring.add(Arc::clone(tx));
    }
    let mut partial = PartiallyDownloadedBlock::new();
    let init = partial.init_data(cmpct, &Pool(pool.to_vec()), &ring);
    let blocktxn = match answer_with {
        Answer::Raw(b) => b.clone(),
        Answer::Missing {
            ..
        } if init != port::ReadStatus::Ok => None,
        Answer::Missing {
            block,
            perturb,
            pick,
        } => answer(&partial, block, *perturb, *pick)
            .map(|txn| blocktxn_payload(block.header().hash(), &txn)),
    };
    // Core decodes every argument, so an undecodable `blocktxn` is refused whatever `InitData`
    // returns.
    let Some(missing) = read_blocktxn(blocktxn.as_deref()) else {
        return (None, blocktxn);
    };
    let fill = (init == port::ReadStatus::Ok).then(|| {
        let (s, b) = partial.fill_block(&missing);
        (status(s), b.map(|b| core_serialize(&b)))
    });
    let node = CompactFill {
        init: status(init),
        fill: fill.as_ref().map(|f| f.0),
        block: fill.and_then(|f| f.1),
    };
    (Some(node), blocktxn)
}

/// Core's result against the node's: the statuses, then the block's bytes.
pub fn judge(core: &CompactFill, node: &CompactFill) -> Compared {
    check(core.init == node.init, "InitData status")?;
    check(core.fill == node.fill, "FillBlock status")?;
    check(core.block == node.block, "block bytes")?;
    Ok(if core.fill == Some(ReadStatus::Ok) {
        Outcome::BothAccept
    } else {
        Outcome::BothReject
    })
}

/// The argument Core refuses first: the arguments again with each prefix kept and the rest
/// empty (no `blocktxn`), as Core decodes them in order and stops at the first it cannot.
fn core_refused(
    oracle: &Oracle,
    cmpct: &[u8],
    pool: &[Vec<u8>],
    extra: &[Vec<u8>],
) -> Result<Arg, Divergence> {
    type Txs<'a> = &'a [Vec<u8>];
    let prefixes: [(Txs, Txs, Arg); 3] =
        [(&[], &[], Arg::Cmpct), (pool, &[], Arg::Pool), (pool, extra, Arg::Extra)];
    for (pool, extra, arg) in prefixes {
        match oracle.partially_downloaded_block(cmpct, pool, extra, None) {
            Err(e) if decode_error(e.kind) => return Ok(arg),
            Ok(_) => {}
            Err(e) if e.kind == ErrorKind::Argument => {}
            Err(_) => return Err(divergence("oracle error")),
        }
    }
    Ok(Arg::Blocktxn)
}

fn decode_error(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::Decode | ErrorKind::Trailing)
}

fn compare_case(oracle: &Oracle, case: Case) -> Result<Agreed, Divergence> {
    let raw_blocktxn = match &case.answer {
        Answer::Raw(b) => b.clone(),
        Answer::Missing {
            ..
        } => None,
    };
    let decoded = (read_cmpct(&case.cmpct), read_txs(&case.pool), read_txs(&case.extra));
    let (node, refused, pool, blocktxn) = match decoded {
        (Some(cmpct), Some(pool), Some(extra)) => {
            let mut seen = BTreeSet::new();
            let pool: Vec<(Vec<u8>, TxRef)> =
                pool.into_iter().filter(|(_, tx)| seen.insert(tx.txid())).collect();
            let txs: Vec<TxRef> = pool.iter().map(|(_, tx)| Arc::clone(tx)).collect();
            let extra: Vec<TxRef> = extra.into_iter().map(|(_, tx)| tx).collect();
            let (node, blocktxn) = node_side(&cmpct, &txs, &extra, &case.answer);
            let refused = node.is_none().then_some(Arg::Blocktxn);
            (node, refused, pool.into_iter().map(|(w, _)| w).collect(), blocktxn)
        }
        (cmpct, pool, _) => {
            let refused = if cmpct.is_none() {
                Arg::Cmpct
            } else if pool.is_none() {
                Arg::Pool
            } else {
                Arg::Extra
            };
            (None, Some(refused), case.pool, raw_blocktxn)
        }
    };
    let core =
        oracle.partially_downloaded_block(&case.cmpct, &pool, &case.extra, blocktxn.as_deref());
    // Core built its mempool, and with it the dependencies that read the chain.
    if core.as_ref().map_or_else(|e| e.kind == ErrorKind::Argument, |_| true) {
        mark_ready(oracle.pin());
    }
    match (core, node) {
        (Ok(core), Some(node)) => Ok(Agreed {
            outcome: judge(&core, &node)?,
            fill: Some(core),
            refused: None,
        }),
        (Err(e), _) if decode_error(e.kind) => {
            let core_refused = core_refused(oracle, &case.cmpct, &pool, &case.extra)?;
            check(refused == Some(core_refused), "decode verdict")?;
            Ok(Agreed {
                outcome: Outcome::BothReject,
                fill: None,
                refused,
            })
        }
        (Err(e), Some(_)) if e.kind == ErrorKind::Argument => Err(divergence("mempool txids")),
        (Err(e), _) if e.kind != ErrorKind::Argument => Err(divergence("oracle error")),
        _ => Err(divergence("decode verdict")),
    }
}

//! `cmpct_reconstruct` (P3 §6.5): `cmpctblock` and `blocktxn` against
//! [`PartiallyDownloadedBlock`] with no oracle, checked by invariants. `node-fuzz`'s model
//! target of that name runs [`check`].
//!
//! The input's first byte picks the mode.
//!
//! - **Built** (even): a block of 1–64 synthetic transactions (a coinbase, then spends) whose
//!   header commits to their merkle root. Each transaction is prefilled, in the mempool, in the
//!   extra-transaction ring or none of these, as one flag byte says; then a nonce; then a
//!   perturbation of the `blocktxn` that answers the missing indexes (none, drop one, add one,
//!   swap two, replace one, or a header whose merkle root is wrong).
//! - **Raw** (odd): a length-prefixed `cmpctblock` payload, then length-prefixed `blocktxn`
//!   payloads, decoded and applied in turn with no mempool until one is `READ_STATUS_INVALID`
//!   (the handler then drops the request).
//!
//! Invariants: nothing panics; `InitData` of a built block is never `READ_STATUS_INVALID`, and is
//! `READ_STATUS_FAILED` only with a repeated short ID or more than 12 in one libstdc++ bucket;
//! `FillBlock`'s `READ_STATUS_OK` carries a
//! block whose merkle root is its header's and that is not mutated (for a built block, the block's
//! own transactions); `READ_STATUS_INVALID` exactly when the `blocktxn` has a different count from
//! the missing indexes; an unperturbed built block with distinct transactions always
//! reconstructs. (§6.5's "`Invalid` ⇒ a score of 100" is the handler's, held by the `sim` tests.)

use std::sync::Arc;

use dash_consensus::block::block_mutation;
use dash_consensus::{Block, Header, Tx, TxRef, decode_tx};
use dash_p2p_core::{BlockTxn, CmpctBlock};
use dash_primitives::{BlockHash, BlockHeader, MerkleRoot, TxHash};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;

use crate::processing::compact::{
    CompactBlock, CompactTxSource, ExtraTxns, NoMempool, PartiallyDownloadedBlock, Prefilled,
    ReadStatus, bucket_count,
};

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

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.bytes())
    }

    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.bytes())
    }

    /// A `u16`-length-prefixed slice, cut short at the input's end.
    fn chunk(&mut self) -> &[u8] {
        let len = usize::from(u16::from_le_bytes(self.bytes()));
        let (chunk, rest) = self.0.split_at(len.min(self.0.len()));
        self.0 = rest;
        chunk
    }
}

/// The mempool in insertion order.
struct Pool(Vec<(TxHash, TxRef)>);

impl CompactTxSource for Pool {
    fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool) {
        for (hash, tx) in &self.0 {
            if !f(hash, tx) {
                return;
            }
        }
    }
}

/// A version 1 transaction spending `prevout` with `script_sig`, paying `value` to `OP_TRUE`;
/// never 64 bytes long (`IsBlockMutated`'s size rule is not what this target is about).
fn tx(prevout: ([u8; 32], u32), script_sig: &[u8], value: u64) -> TxRef {
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
    Arc::new(decode_tx(&mut &raw[..]).expect("a well-formed transaction").into_value())
}

fn coinbase(seed: u32) -> TxRef {
    let mut script = seed.to_le_bytes().to_vec();
    script.push(0x51);
    tx(([0; 32], u32::MAX), &script, 50)
}

fn spend(seed: u32) -> TxRef {
    let mut prevout = [0; 32];
    prevout[..4].copy_from_slice(&seed.to_le_bytes());
    tx((prevout, 0), &[], 42)
}

fn header(merkle_root: MerkleRoot, nonce: u32) -> Header {
    Header::new(BlockHeader {
        version: 0x2000_0000,
        prev_hash: BlockHash::ZERO,
        merkle_root,
        time: 1_700_000_000,
        bits: 0x207f_ffff,
        nonce,
    })
}

/// Whether `v` holds an element twice.
fn has_repeat<T: Ord>(mut v: Vec<T>) -> bool {
    v.sort_unstable();
    v.windows(2).any(|w| w[0] == w[1])
}

/// Whether more than 12 of `ids` share a bucket of `InitData`'s map, sized for them (be:88-105).
fn overfull(ids: &[u64]) -> bool {
    let buckets = bucket_count(ids.len());
    let mut sizes = std::collections::HashMap::new();
    ids.iter().any(|id| {
        let size = sizes.entry(id % buckets).or_insert(0);
        *size += 1;
        *size > 12
    })
}

/// `FillBlock`'s invariants for a block that claims `READ_STATUS_OK`.
fn filled_ok(block: Option<&Block>, want: Option<&[TxRef]>) -> Result<(), &'static str> {
    let block = block.ok_or("ok_without_block")?;
    let (root, _) = block.merkle();
    if root != block.header().raw().merkle_root {
        return Err("ok_merkle_root");
    }
    if block_mutation(block).is_some() {
        return Err("ok_mutated");
    }
    let ids = |txs: &[TxRef]| txs.iter().map(|t| t.instance_hash()).collect::<Vec<_>>();
    match want {
        Some(want) if ids(want) != ids(block.txs()) => Err("ok_other_transactions"),
        _ => Ok(()),
    }
}

/// `FillBlock`'s invariants: `READ_STATUS_INVALID` exactly when the `blocktxn`'s count differs
/// from the missing slots', and an `Ok` block as [`filled_ok`] says.
fn filled(
    status: ReadStatus,
    block: Option<&Block>,
    count_differs: bool,
    want: Option<&[TxRef]>,
) -> Result<(), &'static str> {
    match status {
        ReadStatus::Invalid if count_differs => Ok(()),
        ReadStatus::Invalid => Err("fill_invalid_with_matching_count"),
        _ if count_differs => Err("fill_count_differs_not_invalid"),
        ReadStatus::Ok => filled_ok(block, want),
        ReadStatus::Failed => Ok(()),
    }
}

/// The target's check; the error names the broken invariant.
pub fn check(data: &[u8]) -> Result<(), &'static str> {
    let mut input = Input(data);
    if input.byte() % 2 == 1 {
        raw(input)
    } else {
        built(input)
    }
}

fn raw(mut input: Input<'_>) -> Result<(), &'static str> {
    let Ok(msg) = CmpctBlock::decode(&mut &input.chunk()[..]) else {
        return Ok(());
    };
    let cmpct = CompactBlock::from_msg(msg);
    let mut partial = PartiallyDownloadedBlock::new();
    if partial.init_data(&cmpct, &NoMempool, &ExtraTxns::default()) != ReadStatus::Ok {
        return Ok(());
    }
    while !input.0.is_empty() && partial.header().is_some() {
        let Ok(msg) = BlockTxn::decode(&mut &input.chunk()[..]) else {
            continue;
        };
        let txn: Vec<TxRef> = msg.txs.into_iter().map(|t| Arc::new(Tx::new(t))).collect();
        let missing = (0..partial.tx_count()).filter(|&i| !partial.is_tx_available(i)).count();
        let (status, block) = partial.fill_block(&txn);
        filled(status, block.as_ref(), txn.len() != missing, None)?;
        // The handler drops the request after `Invalid` (Misbehaving 100), so nothing fills again.
        if status == ReadStatus::Invalid {
            break;
        }
    }
    Ok(())
}

fn built(mut input: Input<'_>) -> Result<(), &'static str> {
    let n = 1 + usize::from(input.byte() % 64);
    let mut txs = vec![coinbase(input.u32())];
    txs.extend((1..n).map(|_| spend(input.u32())));
    let flags: Vec<u8> = (0..n).map(|_| input.byte()).collect();
    let nonce = input.u64();
    let perturb = input.byte() % 6;
    let pick = usize::from(input.byte());

    let (root, _) = Block::new(header(MerkleRoot::ZERO, 0), txs.clone()).merkle();
    let root = if perturb == 5 {
        MerkleRoot::from_lendian([0xab; 32])
    } else {
        root
    };
    let block = Block::new(header(root, pick as u32), txs.clone());

    let mut cmpct = CompactBlock::from_block(&block, nonce);
    let prefilled = |i: usize| flags[i] & 1 != 0;
    // Differential indexes, as [`Prefilled::index`] says.
    cmpct.prefilled.clear();
    let mut next = 0;
    for i in (0..n).filter(|&i| prefilled(i)) {
        cmpct.prefilled.push(Prefilled {
            index: (i - next) as u16,
            tx: Arc::clone(&txs[i]),
        });
        next = i + 1;
    }
    cmpct.short_ids = (0..n)
        .filter(|&i| !prefilled(i))
        .map(|i| cmpct.short_id(&txs[i].instance_hash()))
        .collect();
    let pool = Pool(
        (0..n)
            .filter(|&i| flags[i] & 2 != 0)
            .map(|i| (txs[i].instance_hash(), Arc::clone(&txs[i])))
            .collect(),
    );
    let mut extra = ExtraTxns::new(100);
    for i in (0..n).filter(|&i| flags[i] & 4 != 0) {
        extra.add(Arc::clone(&txs[i]));
    }

    let mut partial = PartiallyDownloadedBlock::new();
    let may_fail = has_repeat(cmpct.short_ids.clone()) || overfull(&cmpct.short_ids);
    match partial.init_data(&cmpct, &pool, &extra) {
        ReadStatus::Invalid => return Err("init_invalid"),
        ReadStatus::Failed if may_fail => return Ok(()),
        ReadStatus::Failed => return Err("init_failed_without_cause"),
        ReadStatus::Ok => {}
    }
    let missing: Vec<usize> = (0..n).filter(|&i| !partial.is_tx_available(i)).collect();
    let mut txn: Vec<TxRef> = missing.iter().map(|&i| Arc::clone(&txs[i])).collect();
    match perturb {
        1 if !txn.is_empty() => {
            txn.remove(pick % txn.len());
        }
        2 => txn.insert(pick % (txn.len() + 1), spend(u32::MAX - pick as u32)),
        3 if txn.len() > 1 => {
            let (i, len) = (pick % txn.len(), txn.len());
            txn.swap(i, (i + 1) % len);
        }
        4 if !txn.is_empty() => {
            let i = pick % txn.len();
            txn[i] = spend(u32::MAX - pick as u32);
        }
        _ => {}
    }
    let (status, block) = partial.fill_block(&txn);
    filled(status, block.as_ref(), txn.len() != missing.len(), Some(&txs))?;
    let distinct = !has_repeat(txs.iter().map(|t| t.instance_hash()).collect::<Vec<TxHash>>());
    let unperturbed = perturb == 0
        || (perturb == 1 && missing.is_empty())
        || (perturb == 3 && missing.len() < 2)
        || (perturb == 4 && missing.is_empty());
    if unperturbed && distinct && status != ReadStatus::Ok {
        return Err("unperturbed_not_reconstructed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic sweep over both modes; `node-fuzz` runs the real search.
    #[test]
    fn invariants_hold_on_a_sweep() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for len in (0..600).map(|i| i % 300) {
            let data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if let Err(e) = check(&data) {
                panic!("{e}: {}", crate::sim::transcript::hex(&data));
            }
        }
    }

    /// An unperturbed block of 20 distinct transactions, all missing, reconstructs from its
    /// `blocktxn`; with one dropped it is `Invalid`.
    #[test]
    fn built_cases() {
        let mut data = vec![0, 19];
        data.extend((1..=20u32).flat_map(|s| (s * 7919).to_le_bytes()));
        data.extend([0; 20]);
        data.extend([0; 8]);
        assert_eq!(check(&[&data[..], &[0, 0]].concat()), Ok(()));
        assert_eq!(check(&[&data[..], &[1, 3]].concat()), Ok(()));
    }

    /// Raw mode: a `cmpctblock` with one transaction missing, then a `blocktxn` with none (Sol
    /// r1's case, `Invalid`), and one with the missing transaction (`Ok`, which `check` takes
    /// through `filled_ok`).
    #[test]
    fn raw_cases() {
        let txs = vec![coinbase(1), spend(2)];
        let (root, _) = Block::new(header(MerkleRoot::ZERO, 0), txs.clone()).merkle();
        let block = Block::new(header(root, 0), txs);
        let chunk = |payload: Vec<u8>| {
            let mut out = (payload.len() as u16).to_le_bytes().to_vec();
            out.extend(payload);
            out
        };
        let mut cmpct = Vec::new();
        CompactBlock::from_block(&block, 5).to_msg().encode(&mut cmpct);
        let blocktxn = |txs: &[TxRef]| {
            let mut buf = Vec::new();
            BlockTxn {
                block_hash: block.header().hash(),
                txs: txs.iter().map(|t| t.to_mutable()).collect(),
            }
            .encode(&mut buf);
            buf
        };
        for txn in [&[][..], &block.txs()[1..]] {
            let data = [vec![1], chunk(cmpct.clone()), chunk(blocktxn(txn))].concat();
            assert_eq!(check(&data), Ok(()));
        }
        let mut partial = PartiallyDownloadedBlock::new();
        let c = CompactBlock::from_block(&block, 5);
        assert_eq!(partial.init_data(&c, &NoMempool, &ExtraTxns::default()), ReadStatus::Ok);
        assert_eq!(partial.fill_block(&block.txs()[1..]).0, ReadStatus::Ok);
    }

    /// 13 distinct short IDs in one bucket fail `InitData` and are no broken invariant; 12 in
    /// one bucket are no reason to fail.
    #[test]
    fn overfull_buckets() {
        let b = bucket_count(13);
        assert!(overfull(&(0..13).map(|k| 1 + k * b).collect::<Vec<_>>()));
        let b = bucket_count(14);
        let ids: Vec<u64> = (0..12).map(|k| 1 + k * b).chain([0, 2]).collect();
        assert!(!overfull(&ids));
    }
}

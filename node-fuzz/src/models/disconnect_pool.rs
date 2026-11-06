//! `disconnect_pool` (CH3b b1; CH3 §7.1, D48): the production disconnect pool,
//! [`DisconnectedBlockTransactions`], against a reference model that keeps Core's list and map
//! as plain vectors and recomputes Core's `DynamicMemoryUsage` from scratch after every call.
//! Both read the bucket count from [`Buckets`], each driving its own copy with its own map size;
//! Core's vectors check `Buckets` and the pool's figures on Core's own sequences
//! (`dashd-chainstate/tests/disconnect_pool_vectors.rs`). This target explores the sequences those
//! do not hold: repeated txids, re-signed asset unlocks, and budgets that evict mid-block.
//!
//! An input is a budget, then calls:
//! - byte 0 even: Core's budget, 20,000,000 bytes; odd: the next two bytes (LE) times 8;
//! - then per call a byte `op` and a count byte `n`, then `n % 64` bytes that each pick one of
//!   256 transactions: `op % 8` below 4 adds a block of them (`AddTransactionsFromBlock`), below
//!   7 removes one (`removeForBlock`), and 7 takes the pool (`take()`, which reads its picks and
//!   ignores them).
//!
//! After each call the queue, the evicted or taken transactions (as the same `Arc`s), the
//! accounting and the bucket count must be the model's, and after an add the pool must be within
//! its budget or empty.

use std::sync::{Arc, OnceLock};

use dash_consensus::{Tx, TxRef};
use dash_primitives::{
    ASSET_UNLOCK_PAYLOAD_SIZE, ASSET_UNLOCK_QUORUM_INFO_SIZE, OutPoint, Transaction, TxHash, TxIn,
    TxOut, TxType,
};
use dashd_chainstate::chain::DisconnectPool;
use dashd_chainstate::reorg::buckets::Buckets;
use dashd_chainstate::reorg::memusage::{list_usage, recursive_dynamic_usage, unordered_map_usage};
use dashd_chainstate::reorg::{DisconnectedBlockTransactions, MAX_DISCONNECTED_TX_POOL_SIZE};
use node_l1::Divergence;

/// Script lengths either side of a `CScript`'s inline 28 bytes and of `MallocUsage`'s granules.
const SCRIPT_LENGTHS: [usize; 12] = [0, 1, 27, 28, 29, 44, 45, 61, 62, 107, 520, 1650];

/// The first of the asset unlocks: 224 to 255 are 16 pairs, each one withdrawal signed twice.
const UNLOCKS: u8 = 224;

/// The transactions a pick names. Below [`UNLOCKS`], `i` has `i % 4` inputs and `(i / 4) % 4`
/// outputs of assorted script lengths, and lock time `i`, so every txid differs. From it on,
/// pairs of version-2 asset unlocks differ only in their quorum signing info: one txid
/// (`GetHash()`, the stable txid) for two transactions.
fn universe() -> &'static [TxRef] {
    static UNIVERSE: OnceLock<Vec<TxRef>> = OnceLock::new();
    UNIVERSE.get_or_init(|| (0..=u8::MAX).map(transaction).collect())
}

fn transaction(i: u8) -> TxRef {
    let n = usize::from(i);
    let raw = if i < UNLOCKS {
        Transaction {
            version: 2,
            tx_type: TxType::new(0),
            inputs: (0..n % 4)
                .map(|j| TxIn {
                    prevout: OutPoint {
                        hash: TxHash::from([i; 32]),
                        index: j as u32,
                    },
                    script_sig: vec![0x51; SCRIPT_LENGTHS[(n * 7 + j) % SCRIPT_LENGTHS.len()]]
                        .into(),
                    sequence: u32::MAX,
                })
                .collect(),
            outputs: (0..(n / 4) % 4)
                .map(|j| TxOut {
                    value: i64::from(i),
                    script_pubkey: vec![
                        0x51;
                        SCRIPT_LENGTHS[(n * 5 + j * 3) % SCRIPT_LENGTHS.len()]
                    ]
                    .into(),
                })
                .collect(),
            lock_time: u32::from(i),
            extra_payload: Vec::new(),
        }
    } else {
        let pair = (i - UNLOCKS) / 2;
        let mut payload = vec![pair; ASSET_UNLOCK_PAYLOAD_SIZE];
        payload[0] = 2;
        payload[ASSET_UNLOCK_PAYLOAD_SIZE - ASSET_UNLOCK_QUORUM_INFO_SIZE..].fill(i & 1);
        Transaction {
            version: 3,
            tx_type: TxType::new(9),
            inputs: Vec::new(),
            outputs: vec![TxOut {
                value: i64::from(pair) + 1,
                script_pubkey: vec![0x51; 25].into(),
            }],
            lock_time: 0,
            extra_payload: payload,
        }
    };
    Arc::new(Tx::new(raw))
}

/// Core's pool as plain vectors.
struct Model {
    max: usize,
    /// `queuedTx`, front first: each entry's id and transaction.
    queue: Vec<(u64, TxRef)>,
    /// `iters_by_txid`: each txid's queue entry, in no order.
    map: Vec<(TxHash, u64)>,
    next_id: u64,
    buckets: Buckets,
}

impl Model {
    fn new(max: usize) -> Self {
        Model {
            max,
            queue: Vec::new(),
            map: Vec::new(),
            next_id: 0,
            buckets: Buckets::default(),
        }
    }

    fn usage(&self) -> usize {
        self.inner_usage()
            + unordered_map_usage(self.map.len(), self.buckets.count() as usize)
            + list_usage(self.queue.len())
    }

    fn inner_usage(&self) -> usize {
        self.queue.iter().map(|(_, tx)| recursive_dynamic_usage(tx)).sum()
    }

    fn add(&mut self, vtx: &[TxRef]) -> Vec<TxRef> {
        let len = self.map.len() as u64;
        self.buckets.reserve(len, len + vtx.len() as u64);
        for tx in vtx.iter().rev() {
            let id = self.next_id;
            self.next_id += 1;
            self.queue.push((id, Arc::clone(tx)));
            if !self.map.iter().any(|(txid, _)| *txid == tx.txid()) {
                self.buckets.insert(self.map.len() as u64);
                self.map.push((tx.txid(), id));
            }
        }
        let mut evicted = Vec::new();
        while !self.queue.is_empty() && self.usage() > self.max {
            let (_, tx) = self.queue.remove(0);
            self.map.retain(|(txid, _)| *txid != tx.txid());
            evicted.push(tx);
        }
        evicted
    }

    fn remove(&mut self, vtx: &[TxRef]) {
        if self.queue.is_empty() {
            return;
        }
        for tx in vtx {
            if let Some(at) = self.map.iter().position(|(txid, _)| *txid == tx.txid()) {
                let (_, id) = self.map.swap_remove(at);
                self.queue.retain(|(entry, _)| *entry != id);
            }
        }
    }

    fn take(&mut self) -> Vec<TxRef> {
        self.map.clear();
        self.queue.drain(..).map(|(_, tx)| tx).collect()
    }
}

fn ensure(ok: bool, field: &'static str) -> Result<(), Divergence> {
    if ok {
        Ok(())
    } else {
        Err(Divergence {
            field,
        })
    }
}

/// The same transactions, as the same `Arc`s, in the same order.
fn same<'a>(
    a: impl IntoIterator<Item = &'a TxRef>,
    b: impl IntoIterator<Item = &'a TxRef>,
) -> bool {
    a.into_iter().map(Arc::as_ptr).eq(b.into_iter().map(Arc::as_ptr))
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let Some((&budget, mut rest)) = data.split_first() else {
        return Ok(());
    };
    let max = if budget & 1 == 0 {
        MAX_DISCONNECTED_TX_POOL_SIZE * 1000
    } else {
        let Some((low, more)) = rest.split_first_chunk::<2>() else {
            return Ok(());
        };
        rest = more;
        usize::from(u16::from_le_bytes(*low)) * 8
    };
    let mut pool = DisconnectedBlockTransactions::new(max);
    let mut model = Model::new(max);
    let result = run(&mut pool, &mut model, rest);
    // Core asserts a dropped pool is empty, and so do test builds of this one.
    let _ = pool.take();
    result
}

fn run(
    pool: &mut DisconnectedBlockTransactions,
    model: &mut Model,
    mut rest: &[u8],
) -> Result<(), Divergence> {
    let universe = universe();
    while let Some((&[op, n], more)) = rest.split_first_chunk::<2>() {
        let (picks, more) = more.split_at(usize::from(n % 64).min(more.len()));
        rest = more;
        let vtx: Vec<TxRef> =
            picks.iter().map(|&i| Arc::clone(&universe[usize::from(i)])).collect();
        match op % 8 {
            0..4 => {
                let evicted = pool.add_from_block(&vtx);
                ensure(same(&evicted, &model.add(&vtx)), "evicted")?;
                ensure(pool.is_empty() || pool.dynamic_memory_usage() <= model.max, "over budget")?;
            }
            4..7 => {
                pool.remove_for_block(&vtx);
                model.remove(&vtx);
            }
            _ => ensure(same(&pool.take(), &model.take()), "taken")?,
        }
        ensure(same(pool.queued(), model.queue.iter().map(|(_, tx)| tx)), "queue")?;
        ensure(pool.bucket_count() == model.buckets.count(), "bucket count")?;
        ensure(pool.inner_usage() == model.inner_usage(), "cachedInnerUsage")?;
        ensure(pool.dynamic_memory_usage() == model.usage(), "DynamicMemoryUsage")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The universe's asset unlocks pair up on their stable txid, and nothing else repeats one.
    #[test]
    fn only_the_unlock_pairs_share_a_txid() {
        let universe = universe();
        let txids: std::collections::HashSet<TxHash> =
            universe.iter().map(|tx| tx.txid()).collect();
        assert_eq!(txids.len(), 256 - 16);
        for pair in universe[usize::from(UNLOCKS)..].chunks(2) {
            assert_eq!(pair[0].txid(), pair[1].txid());
            assert_ne!(pair[0].instance_hash(), pair[1].instance_hash());
        }
    }

    /// A re-signed unlock removes the queued one, and adding both keeps the first in the map.
    #[test]
    fn calls_on_a_shared_txid_agree() {
        let input = [
            1, 0x00, 0x02, // a budget of 512 × 8 = 4,096 bytes
            0, 3, 1, 224, 2, // add [1, 224, 2]
            4, 1, 225, // remove [225]: the same txid as 224
            0, 2, 226, 227, // add [226, 227]: one txid
            4, 1, 226, // remove [226]
            0, 40, // add 40 transactions, which the budget evicts
        ];
        let mut data = input.to_vec();
        data.extend(10..50);
        data.extend([7, 0]);
        assert!(check(&data).is_ok());
    }
}

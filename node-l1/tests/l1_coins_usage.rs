//! Core's `CCoinsViewCache::DynamicMemoryUsage()` against [`CoreUsage`] (CH2c D4), op by op,
//! through `oracle_coins_ops`' USAGE (v24.0): the provenance of the model's node size (104 bytes),
//! chunk size (256 KiB, with the first chunk allocated by `PoolResource`'s constructor), bucket
//! policy (libstdc++'s `_Prime_rehash_policy`) and script heap (prevector's 28 inline bytes). The
//! script grows a tip past two chunks and many rehashes, erases (which free nothing), and flushes
//! (`ReallocateCache`), and adds over a held key (the emplace's own node).

use dash_consensus::coins::Coin;
use dash_oracle_sys::coins_ops::{DEFAULT_DB_BATCH_SIZE, OutPoint, TIP};
use dash_oracle_sys::{CoinsOp, CoinsOutcome, CoinsOutput, Oracle, Pin};
use dashd_chainstate::coins::PackedCoin;
use node_l1::core_usage::{CoreUsage, NODES_PER_CHUNK, coin_usage};

fn outpoint(i: u32) -> OutPoint {
    let mut txid = [7; 32];
    txid[..4].copy_from_slice(&i.to_le_bytes());
    OutPoint {
        txid,
        vout: i % 3,
    }
}

/// A coin whose script is `len` bytes of `OP_TRUE` (stored raw, so its decompressed size is `len`).
fn coin(len: usize) -> Vec<u8> {
    PackedCoin::from_coin(Coin::new(5, &vec![0x51; len], 3, false).as_ref()).as_bytes().to_vec()
}

/// The script's ops, each with the model's state after it.
struct Script {
    ops: Vec<CoinsOp>,
    model: Vec<CoreUsage>,
    usage: CoreUsage,
}

impl Script {
    fn push(&mut self, op: CoinsOp, change: impl FnOnce(&mut CoreUsage)) {
        change(&mut self.usage);
        self.ops.push(op);
        self.ops.push(CoinsOp::Usage {
            view: TIP,
        });
        self.model.push(self.usage.clone());
    }

    fn add(&mut self, i: u32, len: usize) {
        let c = coin(len);
        let op = CoinsOp::Add {
            view: TIP,
            outpoint: outpoint(i),
            coin: c.clone(),
            possible_overwrite: false,
        };
        self.push(op, |u| {
            u.insert();
            u.add_coin(&c);
        });
    }

    /// A spend of a FRESH coin: its entry is erased.
    fn spend(&mut self, i: u32, len: usize) {
        let c = coin(len);
        let op = CoinsOp::Spend {
            view: TIP,
            outpoint: outpoint(i),
            moveout: false,
        };
        self.push(op, |u| {
            u.sub_coin(&c);
            u.erase();
        });
    }
}

#[test]
fn usage_follows_core() {
    let mut s = Script {
        ops: Vec::new(),
        model: Vec::new(),
        usage: CoreUsage::new(),
    };
    let chunk = u32::try_from(NODES_PER_CHUNK).unwrap();
    // Past the first chunk and through the bucket array's growth, P2PKH-sized scripts (inline).
    for i in 0..chunk + 40 {
        s.add(i, 25);
    }
    // Every script-size class around prevector's inline capacity.
    for (j, len) in [26, 28, 29, 30, 40, 47, 48, 49, 64, 100, 520].into_iter().enumerate() {
        s.add(100_000 + j as u32, len);
    }
    // Erases keep the chunks and the buckets; inserts then reuse the freed nodes.
    for i in 0..300 {
        s.spend(i, 25);
    }
    for i in 200_000..200_300 {
        s.add(i, 33);
    }
    // A flush empties the map and reallocates it: one bucket, the constructor's chunk.
    s.push(
        CoinsOp::SetBestBlock {
            view: TIP,
            hash: [1; 32],
        },
        |_| {},
    );
    s.push(
        CoinsOp::Flush {
            view: TIP,
        },
        CoreUsage::reallocate,
    );
    s.add(300_000, 25);
    // A full chunk, then `AddCoin` over a key the map holds: libstdc++ allocates the emplace's
    // node before its lookup, and with the free list empty that node is a second chunk's first.
    for i in 1..chunk {
        s.add(300_000 + i, 25);
    }
    let (old, new) = (coin(25), coin(40));
    s.push(
        CoinsOp::Add {
            view: TIP,
            outpoint: outpoint(300_000),
            coin: new.clone(),
            possible_overwrite: true,
        },
        |u| {
            u.emplace_existing();
            u.sub_coin(&old);
            u.add_coin(&new);
        },
    );
    let outcomes = Oracle::new(Pin::V24_0).coins_ops(&s.ops, DEFAULT_DB_BATCH_SIZE).unwrap();
    assert_eq!(outcomes.len(), s.ops.len());
    for (k, model) in s.model.iter().enumerate() {
        match &outcomes[2 * k + 1] {
            CoinsOutcome::Ok(CoinsOutput::Usage {
                dynamic_usage,
                cache_size,
            }) => {
                assert_eq!(
                    (*dynamic_usage, u64::from(*cache_size)),
                    (model.dynamic_usage(), model.cache_size()),
                    "op {k} ({:?})",
                    s.ops[2 * k]
                );
            }
            other => panic!("op {k}: {other:?}"),
        }
    }
    // The script crossed into a second chunk, and reached the large scripts.
    assert!(s.model.iter().any(|u| u.dynamic_usage() > 2 * 262_144));
    assert_eq!(coin_usage(&coin(520)), 544);
}

//! The harness's own books (plan §3.2, §3.3): each leg's UTXO set from the blocks it saw
//! connect, its MuHash for P5, the mempool dashd must hold, and the facts the binder measures
//! from a block's bytes.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::coins::stats::{CoinStatsHashType, UtxoSetScan};
use dash_consensus::coins::{Coin, CoinRef, is_unspendable};
use dash_primitives::{BlockHash, OutPoint, TxHash};

use crate::chain::build::{BlockPlan, TxPlan};
use crate::chain::mutate::SEQUENCE_DISABLE;
use crate::chain::world::Outpoint;

/// A coin as the ledger keeps it: value, scriptPubKey, height, coinbase.
pub type LedgerCoin = (i64, Vec<u8>, u32, bool);

/// Whether `tx` is a coinbase (`IsCoinBase`: one input, null prevout).
pub fn is_coinbase(tx: &TxPlan) -> bool {
    tx.inputs.len() == 1 && tx.inputs[0].prevout == ([0; 32], u32::MAX)
}

/// One leg's UTXO set, as the blocks that connected left it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    pub coins: BTreeMap<Outpoint, LedgerCoin>,
}

impl Ledger {
    /// `UpdateCoins` for a block at `height` connected: every input's coin spent, every
    /// spendable output added. `Err` for an input the ledger does not hold: the node connected a
    /// block the harness thinks spends nothing.
    pub fn connect(
        &mut self,
        plan: &BlockPlan,
        txids: &[[u8; 32]],
        height: u32,
    ) -> Result<(), String> {
        for (tx, txid) in plan.txs.iter().zip(txids) {
            let coinbase = is_coinbase(tx);
            if !coinbase {
                for input in &tx.inputs {
                    if self.coins.remove(&input.prevout).is_none() {
                        return Err(format!(
                            "a connected block at {height} spends {}:{}, which the ledger lacks",
                            super::trial::hex(&input.prevout.0),
                            input.prevout.1
                        ));
                    }
                }
            }
            for (n, (value, script)) in tx.outputs.iter().enumerate() {
                // Core: coins.cpp:83-96 AddCoins (unspendable outputs are never added)
                if !is_unspendable(script) {
                    self.coins
                        .insert((*txid, n as u32), (*value, script.clone(), height, coinbase));
                }
            }
        }
        Ok(())
    }

    /// MuHash3072 of the set, internal byte order (`gettxoutsetinfo muhash` prints it reversed).
    pub fn muhash(&self) -> [u8; 32] {
        muhash(self.coins.iter().map(|(o, (value, script, height, coinbase))| {
            (outpoint(o), Coin::new(*value, script, *height, *coinbase))
        }))
    }
}

/// The P5 hash over any coins: `ComputeUTXOStats`' MuHash (no best block in it).
pub fn muhash(coins: impl IntoIterator<Item = (OutPoint, Coin)>) -> [u8; 32] {
    let mut scan = UtxoSetScan::new(CoinStatsHashType::MuHash, &BlockHash::from([0; 32]));
    for (o, c) in coins {
        scan.push(&o, &c.as_ref());
    }
    scan.finish().hash.expect("MuHash has a hash")
}

/// The same over borrowed coins (V's `tip_coins`).
pub fn muhash_refs<'a>(coins: impl IntoIterator<Item = (OutPoint, CoinRef<'a>)>) -> [u8; 32] {
    let mut scan = UtxoSetScan::new(CoinStatsHashType::MuHash, &BlockHash::from([0; 32]));
    for (o, c) in coins {
        scan.push(&o, &c);
    }
    scan.finish().hash.expect("MuHash has a hash")
}

pub fn outpoint(o: &Outpoint) -> OutPoint {
    OutPoint {
        hash: TxHash::from(o.0),
        index: o.1,
    }
}

/// The mempool dashd must hold (plan §3.3): empty until a rollback re-adds a block's
/// transactions; a connected block removes what it confirms, what conflicts with it and their
/// descendants.
#[derive(Clone, Debug, Default)]
pub struct Mempool {
    pub txs: BTreeMap<[u8; 32], TxPlan>,
}

impl Mempool {
    pub fn txids(&self) -> BTreeSet<[u8; 32]> {
        self.txs.keys().copied().collect()
    }

    /// What `removeForBlock` takes for a block, each with its reason: the block's transactions,
    /// those spending an outpoint it spends, and every descendant of either.
    pub fn removed_for_block(
        &self,
        plan: &BlockPlan,
        txids: &[[u8; 32]],
    ) -> BTreeMap<[u8; 32], Removal> {
        let spent: BTreeSet<Outpoint> =
            plan.txs.iter().flat_map(|t| t.inputs.iter().map(|i| i.prevout)).collect();
        let mut gone: BTreeMap<[u8; 32], Removal> = BTreeMap::new();
        for (id, tx) in &self.txs {
            if txids.contains(id) {
                gone.insert(*id, Removal::Confirmed);
            } else if let Some(i) = tx.inputs.iter().find(|i| spent.contains(&i.prevout)) {
                gone.insert(*id, Removal::Conflict(i.prevout));
            }
        }
        loop {
            let more: Vec<([u8; 32], Removal)> = self
                .txs
                .iter()
                .filter(|(id, _)| !gone.contains_key(*id))
                .filter_map(|(id, tx)| {
                    let parent = tx.inputs.iter().find(|i| gone.contains_key(&i.prevout.0))?;
                    Some((*id, Removal::Descendant(parent.prevout.0)))
                })
                .collect();
            if more.is_empty() {
                break;
            }
            gone.extend(more);
        }
        gone
    }

    /// `removeForBlock`.
    pub fn remove_for_block(&mut self, plan: &BlockPlan, txids: &[[u8; 32]]) {
        let gone = self.removed_for_block(plan, txids);
        self.txs.retain(|id, _| !gone.contains_key(id));
    }
}

/// Why a connected block took a transaction from the mempool (OD-L3-21).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    Confirmed,
    /// It spends this outpoint, which the block spends.
    Conflict(Outpoint),
    /// It spends an output of this removed transaction.
    Descendant([u8; 32]),
}

impl std::fmt::Display for Removal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Removal::Confirmed => write!(f, "confirmed"),
            Removal::Conflict((hash, n)) => {
                write!(f, "conflict on {}:{n}", super::trial::hex(hash))
            }
            Removal::Descendant(parent) => write!(f, "descendant of {}", super::trial::hex(parent)),
        }
    }
}

/// `IsFinalTx(tx, height, cutoff)`.
// Core: consensus/tx_verify.cpp:17-35
fn is_final(tx: &TxPlan, height: u32, cutoff: u32) -> bool {
    const LOCKTIME_THRESHOLD: u32 = 500_000_000;
    if tx.lock_time == 0 {
        return true;
    }
    let limit = if tx.lock_time < LOCKTIME_THRESHOLD {
        height
    } else {
        cutoff
    };
    if tx.lock_time < limit {
        return true;
    }
    tx.inputs.iter().all(|i| i.sequence == u32::MAX)
}

/// The facts plan §3.5 step 3 measures from a block at `height` on a parent whose median time
/// past is `mtp`, with every BIP deployment of L3's legs active (CSV's at 150, below every
/// case): `nonfinal_locktime` (a transaction not final under BIP113's cutoff) and `bip68` (a
/// height lock unmet on chain-only coin heights). A fact the bytes and `coins` cannot decide (a
/// time lock, an input the ledger lacks) is left out, which keeps its rows unbound.
pub fn measure(
    plan: &BlockPlan,
    txids: &[[u8; 32]],
    height: u32,
    mtp: u32,
    coins: &Ledger,
) -> BTreeMap<&'static str, bool> {
    // Core: validation.cpp:4532-4541 ContextualCheckBlock (BIP113's cutoff)
    let nonfinal = plan.txs.iter().any(|tx| !is_final(tx, height, mtp));
    let mut measured = BTreeMap::from([("nonfinal_locktime", nonfinal)]);
    // Core: consensus/tx_verify.cpp:39-104 CalculateSequenceLocks, EvaluateSequenceLocks
    const TYPE_FLAG: u32 = 1 << 22;
    const MASK: u32 = 0x0000_ffff;
    let mut unmet = false;
    let mut undecided = false;
    for (t, tx) in plan.txs.iter().enumerate() {
        // Core casts the version to unsigned: -1 is enforced (tx_verify.cpp:55-56).
        if is_coinbase(tx) || (tx.version as i32 as u32) < 2 {
            continue;
        }
        for input in &tx.inputs {
            if input.sequence & SEQUENCE_DISABLE != 0 {
                continue;
            }
            if input.sequence & TYPE_FLAG != 0 {
                undecided = true;
                continue;
            }
            let coin_height = match coins.coins.get(&input.prevout) {
                Some(c) => c.2,
                // A coin of an earlier transaction in the block is at the block's height.
                None if txids[..t].contains(&input.prevout.0) => height,
                None => {
                    undecided = true;
                    continue;
                }
            };
            let lock = input.sequence & MASK;
            // nMinHeight = coin height + lock - 1 must be below the block's height.
            if coin_height + lock > height {
                unmet = true;
            }
        }
    }
    if unmet || !undecided {
        measured.insert("bip68", unmet);
    }
    measured
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::build::{Input, Lock, coinbase};

    fn spend(prevout: Outpoint, sequence: u32, lock_time: u32) -> TxPlan {
        let mut tx = TxPlan::simple(
            vec![Input::new(prevout, Vec::new())],
            vec![(1_000, Lock::True.script_pubkey())],
        );
        tx.inputs[0].sequence = sequence;
        tx.lock_time = lock_time;
        tx
    }

    fn block(txs: Vec<TxPlan>) -> (BlockPlan, Vec<[u8; 32]>) {
        let plan = BlockPlan {
            prev: [0; 32],
            version: 4,
            time: 0,
            bits: 0,
            txs,
            merkle_override: None,
        };
        let txids = plan.txs.iter().map(TxPlan::txid).collect();
        (plan, txids)
    }

    /// Connecting spends and adds; OP_RETURN outputs never enter; MuHash follows the set.
    #[test]
    fn the_ledger_connects_blocks() {
        let mut ledger = Ledger::default();
        let empty = ledger.muhash();
        let (b1, ids1) = block(vec![coinbase(1, 5_000, 1)]);
        ledger.connect(&b1, &ids1, 1).expect("a coinbase");
        assert_eq!(ledger.coins[&(ids1[0], 0)], (5_000, vec![0x51], 1, true));
        let mut tx = spend((ids1[0], 0), u32::MAX, 0);
        tx.outputs.push((0, vec![0x6a, 0x01, 0x07]));
        let (b2, ids2) = block(vec![coinbase(2, 5_000, 2), tx]);
        ledger.connect(&b2, &ids2, 2).expect("a spend");
        assert!(!ledger.coins.contains_key(&(ids1[0], 0)));
        assert!(ledger.coins.contains_key(&(ids2[1], 0)));
        assert!(!ledger.coins.contains_key(&(ids2[1], 1)), "unspendable");
        assert_ne!(ledger.muhash(), empty);
        let e = ledger.connect(&b2, &ids2, 3).expect_err("spent twice");
        assert!(e.contains("lacks"), "{e}");
    }

    /// The facts: a height lock met or not on the ledger's heights, a time lock undecided, and
    /// BIP113's cutoff.
    #[test]
    fn measured_facts_come_from_the_bytes_and_the_ledger() {
        let mut ledger = Ledger::default();
        ledger.coins.insert(([1; 32], 0), (5_000, vec![0x51], 100, true));
        let measure_one = |tx: TxPlan| {
            let (plan, ids) = block(vec![coinbase(200, 1, 1), tx]);
            measure(&plan, &ids, 200, 1_700_000_000, &ledger)
        };
        let m = measure_one(spend(([1; 32], 0), 1000, 0));
        assert_eq!((m["bip68"], m["nonfinal_locktime"]), (true, false));
        let m = measure_one(spend(([1; 32], 0), 100, 0));
        assert!(!m["bip68"], "100 + 100 - 1 < 200");
        let m = measure_one(spend(([1; 32], 0), 101, 0));
        assert!(m["bip68"], "100 + 101 - 1 >= 200");
        let m = measure_one(spend(([1; 32], 0), 1 << 22 | 1, 0));
        assert_eq!(m.get("bip68"), None, "a time lock is not measured");
        let m = measure_one(spend(([9; 32], 0), 5, 0));
        assert_eq!(m.get("bip68"), None, "a coin the ledger lacks");
        let m = measure_one(spend(([1; 32], 0), 0, 200));
        assert_eq!((m["nonfinal_locktime"], m["bip68"]), (true, false));
        let m = measure_one(spend(([1; 32], 0), u32::MAX, 200));
        assert!(!m["nonfinal_locktime"], "final sequences");
        let m = measure_one(spend(([1; 32], 0), 0, 1_700_000_000));
        assert!(m["nonfinal_locktime"], "a time lock at the cutoff is not final");
        let m = measure_one(spend(([1; 32], 0), 0, 1_699_999_999));
        assert!(!m["nonfinal_locktime"]);
    }

    /// A block removes its own transactions, its conflicts and their descendants.
    #[test]
    fn a_block_clears_its_conflicts_from_the_mempool() {
        let a = spend(([1; 32], 0), u32::MAX, 0);
        let child = spend((a.txid(), 0), u32::MAX, 0);
        let other = spend(([2; 32], 0), u32::MAX, 0);
        let mut pool = Mempool::default();
        for tx in [&a, &child, &other] {
            pool.txs.insert(tx.txid(), tx.clone());
        }
        let mut rival = spend(([1; 32], 0), u32::MAX, 0);
        rival.outputs[0].0 = 999;
        let (plan, ids) = block(vec![coinbase(5, 1, 1), rival]);
        let reasons = pool.removed_for_block(&plan, &ids);
        assert_eq!(reasons[&a.txid()], Removal::Conflict(([1; 32], 0)));
        assert_eq!(reasons[&child.txid()], Removal::Descendant(a.txid()));
        assert_eq!(
            reasons[&child.txid()].to_string(),
            format!("descendant of {}", super::super::trial::hex(&a.txid()))
        );
        assert_eq!(reasons.len(), 2);
        let (mine, mine_ids) = block(vec![coinbase(5, 1, 1), other.clone()]);
        assert_eq!(pool.removed_for_block(&mine, &mine_ids)[&other.txid()], Removal::Confirmed);
        pool.remove_for_block(&plan, &ids);
        assert_eq!(pool.txids(), BTreeSet::from([other.txid()]));
    }

    /// m-1: BIP68 applies from version 2 as Core reads it, unsigned, so -1 is enforced.
    #[test]
    fn bip68_reads_the_version_unsigned() {
        let mut ledger = Ledger::default();
        ledger.coins.insert(([1; 32], 0), (5_000, vec![0x51], 100, true));
        for (version, enforced) in [(-1, true), (0, false), (1, false), (2, true)] {
            let mut tx = spend(([1; 32], 0), 1000, 0);
            tx.version = version;
            let (plan, ids) = block(vec![coinbase(200, 1, 1), tx]);
            let m = measure(&plan, &ids, 200, 1_700_000_000, &ledger);
            assert_eq!(m["bip68"], enforced, "version {version}");
        }
    }
}

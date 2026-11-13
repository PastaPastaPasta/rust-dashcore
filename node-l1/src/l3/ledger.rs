//! The harness's own books (plan §3.2, §3.3): each leg's UTXO set from the blocks it saw
//! connect, its MuHash for P5, the mempool dashd must hold, and the facts the binder measures
//! from a block's bytes.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::coins::stats::{CoinStatsHashType, UtxoSetScan};
use dash_consensus::coins::{Coin, CoinRef, is_unspendable};
use dash_primitives::{BlockHash, OutPoint, TxHash};

use crate::chain::build::{BlockPlan, TxPlan, tx_size};
use crate::chain::mutate::{CSV_HEIGHT, DIP0001_HEIGHT, SEQUENCE_DISABLE, block_size};
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
    /// Each connected block's time, by height: BIP68's time locks read their coins' MTPs.
    pub times: BTreeMap<u32, u32>,
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
        self.times.insert(height, plan.time);
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

    /// The median time past at `height` (`GetMedianTimePast`: the median of its block's time
    /// and the ten before), or `None` when one of them was never connected.
    pub fn mtp(&self, height: u32) -> Option<u32> {
        // Core: chain.h:283-295 CBlockIndex::GetMedianTimePast
        let mut times: Vec<u32> = (height.saturating_sub(10)..=height)
            .map(|h| self.times.get(&h).copied())
            .collect::<Option<_>>()?;
        times.sort_unstable();
        Some(times[times.len() / 2])
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
    OutPoint::new(TxHash::from(o.0), o.1)
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
/// past is `mtp`, on L3's legs' activation heights: `nonfinal_locktime` (a transaction not final
/// under the height's cutoff, BIP113's from CSV's height) and `bip68` (a relative lock unmet on
/// chain-only coin heights and the ledger's block times, false below CSV's height). A fact the
/// bytes and `coins` cannot decide (an input the ledger lacks, a time the ledger never saw) is
/// left out, which keeps its rows unbound.
pub fn measure(
    plan: &BlockPlan,
    txids: &[[u8; 32]],
    height: u32,
    mtp: u32,
    coins: &Ledger,
) -> BTreeMap<&'static str, bool> {
    // Core: validation.cpp:4547-4556 ContextualCheckBlock (BIP113's cutoff from CSV)
    let csv = height >= CSV_HEIGHT;
    let cutoff = if csv {
        mtp
    } else {
        plan.time
    };
    let nonfinal = plan.txs.iter().any(|tx| !is_final(tx, height, cutoff));
    let mut measured = BTreeMap::from([("nonfinal_locktime", nonfinal)]);
    // Core: validation.cpp:2637-2639 (BIP68 from CSV)
    let bip68 = if csv {
        bip68_unmet(plan, txids, height, coins)
    } else {
        Some(false)
    };
    if let Some(unmet) = bip68 {
        measured.insert("bip68", unmet);
    }
    measured.extend(limits(plan, txids, height, coins));
    measured
}

/// Whether a relative lock of the block is unmet, `None` when no unmet one is found and one is
/// undecided.
fn bip68_unmet(plan: &BlockPlan, txids: &[[u8; 32]], height: u32, coins: &Ledger) -> Option<bool> {
    // Core: consensus/tx_verify.cpp:39-104 CalculateSequenceLocks, EvaluateSequenceLocks
    const TYPE_FLAG: u32 = 1 << 22;
    const MASK: u32 = 0x0000_ffff;
    const GRANULARITY: u32 = 9;
    let mtp = coins.mtp(height.saturating_sub(1));
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
            if input.sequence & TYPE_FLAG != 0 {
                // nMinTime = MTP(coin height - 1) + (lock << 9) - 1 must be below the parent's
                // MTP.
                match (coins.mtp(coin_height.saturating_sub(1)), mtp) {
                    (Some(coin_mtp), Some(mtp)) => {
                        if i64::from(coin_mtp) + i64::from(lock << GRANULARITY) > i64::from(mtp) {
                            unmet = true;
                        }
                    }
                    _ => undecided = true,
                }
            } else if coin_height + lock > height {
                // nMinHeight = coin height + lock - 1 must be below the block's height.
                unmet = true;
            }
        }
    }
    (unmet || !undecided).then_some(unmet)
}

/// The limit facts (plan §3.5 step 3: serialized sizes, legacy and P2SH sigops), each true only
/// at the site that fails first in Core's order: CheckBlock (DIP0001's limits whatever the
/// height: v:4404-4405, v:4422 via tc:33, v:4432), then ContextualCheckBlock (the height's:
/// v:4562-4564, v:4578 via v:388, v:4585), then ConnectBlock's legacy and P2SH count (v:2752).
/// `sigops_connect` is left out when an input's coin is unknown and the legacy count alone does
/// not decide it.
fn limits(
    plan: &BlockPlan,
    txids: &[[u8; 32]],
    height: u32,
    coins: &Ledger,
) -> BTreeMap<&'static str, bool> {
    use dash_consensus::script::sigops::{legacy_sigop_count, script_sigop_count_with_sig};
    // Core: consensus/consensus.h MaxBlockSize, MaxBlockSigOps; validation.cpp:388
    let dip1 = height >= DIP0001_HEIGHT;
    let (size_max, size_here) = (
        2_000_000,
        if dip1 {
            2_000_000
        } else {
            1_000_000
        },
    );
    let (sigops_max, sigops_here) = (size_max / 50, size_here / 50);
    let size = block_size(plan);
    let count = plan.txs.len();
    let block_over = |max: usize| count == 0 || count > max || size > max;
    let tx_max = plan.txs.iter().any(|t| tx_size(t) > 1_000_000);
    let legacy: usize = plan.txs.iter().map(|t| legacy_sigop_count(t.tx().raw()) as usize).sum();
    let mut out = BTreeMap::from([
        ("block_size_max", block_over(size_max)),
        ("block_size_dip1", block_over(size_here) && !block_over(size_max)),
        ("tx_size_max", tx_max),
        ("tx_size_ctx", dip1 && !tx_max && plan.txs.iter().any(|t| tx_size(t) > 100_000)),
        ("sigops_legacy_max", legacy > sigops_max),
        ("sigops_dip1", legacy > sigops_here && legacy <= sigops_max),
    ]);
    if legacy > sigops_here {
        out.insert("sigops_connect", false);
        return out;
    }
    // Spent scripts: the ledger's coins, or outputs of earlier transactions in the block.
    let mut scripts: BTreeMap<Outpoint, &[u8]> =
        coins.coins.iter().map(|(o, c)| (*o, c.1.as_slice())).collect();
    let mut p2sh = 0;
    for (tx, txid) in plan.txs.iter().zip(txids) {
        if !is_coinbase(tx) {
            for input in &tx.inputs {
                let Some(script) = scripts.get(&input.prevout) else {
                    return out;
                };
                p2sh += script_sigop_count_with_sig(script, &input.script_sig) as usize
                    * usize::from(is_p2sh(script));
            }
        }
        for (n, (_, script)) in tx.outputs.iter().enumerate() {
            scripts.insert((*txid, n as u32), script);
        }
    }
    out.insert("sigops_connect", legacy + p2sh > sigops_here);
    out
}

/// `CScript::IsPayToScriptHash`.
fn is_p2sh(script: &[u8]) -> bool {
    script.len() == 23 && script[0] == 0xa9 && script[1] == 0x14 && script[22] == 0x87
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
    fn the_limit_facts_fire_at_the_first_failing_site() {
        use crate::chain::build::{fill_tx, legacy_sigops};
        let fired = |txs: Vec<TxPlan>, height: u32, ledger: &Ledger| -> Vec<&'static str> {
            let (plan, ids) = block(txs);
            let m = limits(&plan, &ids, height, ledger);
            m.into_iter().filter(|(_, v)| *v).map(|(k, _)| k).collect()
        };
        let with = |n: usize| {
            let mut tx = spend(([1; 32], 0), u32::MAX, 0);
            while tx.outputs.len() < n.div_ceil(10_000) + 1 {
                let left = n - (tx.outputs.len() - 1) * 10_000;
                tx.outputs.push((0, legacy_sigops(left.min(10_000))));
            }
            tx
        };
        let none = Ledger::default();
        assert!(fired(vec![with(20_000)], 115, &none).is_empty());
        assert_eq!(fired(vec![with(20_001)], 115, &none), ["sigops_dip1"]);
        assert_eq!(fired(vec![with(40_001)], 300, &none), ["sigops_legacy_max"]);
        assert_eq!(fired(vec![with(40_001)], 115, &none), ["sigops_legacy_max"]);
        // 39,988 legacy and one spend of the 13-sigop P2SH filler, from the ledger.
        let filler = Lock::Sigops(13);
        let mut ledger = Ledger::default();
        ledger.coins.insert(([2; 32], 0), (5_000, filler.script_pubkey(), 1, false));
        ledger.coins.insert(([1; 32], 0), (5_000, vec![0x51], 1, false));
        let p2sh = TxPlan::simple(
            vec![Input::new(([2; 32], 0), filler.script_sig())],
            vec![(1, vec![0x51])],
        );
        let (plan, ids) = block(vec![with(39_987), p2sh.clone()]);
        assert_eq!(limits(&plan, &ids, 300, &ledger).get("sigops_connect"), Some(&false));
        assert_eq!(fired(vec![with(39_988), p2sh.clone()], 300, &ledger), ["sigops_connect"]);
        let (plan, ids) = block(vec![with(39_988), p2sh]);
        assert!(!limits(&plan, &ids, 300, &none).contains_key("sigops_connect"), "an unknown coin");
        let sized = |n: usize| fill_tx(&spend(([3; 32], 0), u32::MAX, 0), n).expect("fills");
        assert_eq!(fired(vec![sized(100_001)], 300, &none), ["tx_size_ctx"]);
        assert!(fired(vec![sized(100_001)], 115, &none).is_empty());
        assert_eq!(fired(vec![sized(1_000_001)], 300, &none), ["tx_size_max"]);
        assert_eq!(fired(vec![sized(1_000_001)], 115, &none), ["block_size_dip1", "tx_size_max"]);
    }

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

    /// Below CSV's height a time lock's cutoff is the block's time and BIP68 is off.
    #[test]
    fn below_csv_the_cutoff_is_the_block_time() {
        let mut ledger = Ledger::default();
        ledger.coins.insert(([1; 32], 0), (5_000, vec![0x51], 100, true));
        let (mtp, time) = (1_700_000_000, 1_700_000_900);
        let measure_at = |height: u32, tx: TxPlan| {
            let (mut plan, ids) = block(vec![coinbase(height, 1, 1), tx]);
            plan.time = time;
            measure(&plan, &ids, height, mtp, &ledger)
        };
        let in_interval = || spend(([1; 32], 0), u32::MAX - 1, time - 1);
        let below = measure_at(CSV_HEIGHT - 1, in_interval());
        assert!(!below["nonfinal_locktime"], "final against the block's time");
        let at = measure_at(CSV_HEIGHT, in_interval());
        assert!(at["nonfinal_locktime"], "nonfinal against the parent's MTP");
        assert!(measure_at(CSV_HEIGHT - 1, spend(([1; 32], 0), 0, time))["nonfinal_locktime"]);
        let unmet = || spend(([1; 32], 0), 1000, 0);
        assert!(!measure_at(CSV_HEIGHT - 1, unmet())["bip68"], "BIP68 is off");
        assert!(measure_at(CSV_HEIGHT, unmet())["bip68"]);
    }

    /// Group X's blocks give the facts their expectations bind on: a lock refused at
    /// ContextualCheckBlock is nonfinal, one refused at ConnectBlock is an unmet BIP68 lock (and
    /// one block less would meet it), every other variant neither, on the ledger's heights and
    /// times alone.
    #[test]
    fn group_x_facts_match_their_expectations() {
        use crate::chain::build::REGTEST_BITS;
        use crate::chain::mutate::{Cx, Expect, L3_OPERATORS, L3Cx, Mode, Par, Site};
        use crate::chain::world::Coin;
        const T0: u32 = 1_700_000_000;
        let height = 250;
        let mut ledger = Ledger {
            times: (0..height).map(|h| (h, T0 + 150 * h)).collect(),
            ..Ledger::default()
        };
        let coin = Coin {
            outpoint: ([1; 32], 0),
            value: 1_000_000,
            lock: Lock::True,
            height: 101,
            coinbase: false,
        };
        ledger.coins.insert(coin.outpoint, (coin.value, vec![0x51], coin.height, false));
        let mtp = ledger.mtp(height - 1).expect("the parent's MTP");
        let group_x = [
            "locktime",
            "bip68_blocks",
            "bip68_time",
            "bip68_off",
            "tx_version",
            "tx_version_type",
            "header_version",
            "special_tx_type",
            "special_cb_type",
            "cb_type5",
            "v3_type0",
        ];
        for op in L3_OPERATORS.iter().filter(|o| group_x.contains(&o.name)) {
            for v in op.variants {
                let l3 = L3Cx {
                    trial: *b"trial-x\0",
                    carrier: v.carrier,
                    variant: v.name,
                    site: Site::Reg,
                    reward: 5_000,
                    sb_limit: 0,
                    spent: ([6; 32], 1),
                    mature: coin,
                    coin_mtp: ledger.mtp(coin.height - 1).expect("coin 0's MTP"),
                    reuse: &[],
                };
                let coins = [coin, coin, coin];
                let cx = Cx {
                    height,
                    mtp,
                    mocktime: i64::from(T0 + 150 * height),
                    coins: &coins,
                    immature: Some(coin),
                    l3: Some(&l3),
                    prior: &[],
                };
                let mut plan = BlockPlan {
                    prev: [0; 32],
                    version: 0x2000_0000,
                    time: T0 + 150 * height,
                    bits: REGTEST_BITS,
                    txs: vec![coinbase(height, 5_000, 1)],
                    merkle_override: None,
                };
                (op.apply)(&mut plan, &cx);
                let ids: Vec<_> = plan.txs.iter().map(TxPlan::txid).collect();
                let m = measure(&plan, &ids, height, mtp, &ledger);
                let at = |function: &str| match (v.expect)(Mode::Submit, Par::One, Site::Reg) {
                    Expect::Reject {
                        reason,
                        occurrence,
                    } => reason == "bad-txns-nonfinal" && occurrence.contains(function),
                    _ => false,
                };
                let id = format!("{}.{}", op.name, v.name);
                assert_eq!(m["nonfinal_locktime"], at("ContextualCheckBlock#"), "{id}");
                assert_eq!(m.get("bip68"), Some(&at("ConnectBlock#")), "{id}");
                if at("ConnectBlock#") {
                    let mut one_less = plan.clone();
                    one_less.txs[1].inputs[0].sequence -= 1;
                    let ids: Vec<_> = one_less.txs.iter().map(TxPlan::txid).collect();
                    let m = measure(&one_less, &ids, height, mtp, &ledger);
                    assert_eq!(m.get("bip68"), Some(&false), "{id}: unmet by more than one");
                }
            }
        }
    }

    /// The ledger's MTP is Core's: the median of the block's time and the ten before, unknown
    /// while one is missing.
    #[test]
    fn the_ledger_mtp_is_the_median_of_eleven() {
        let mut ledger = Ledger {
            times: (0..20).map(|h| (h, [5, 1, 9, 3, 7][h as usize % 5] + h)).collect(),
            ..Ledger::default()
        };
        let mut window: Vec<u32> = (5..=15).map(|h| ledger.times[&h]).collect();
        window.sort_unstable();
        assert_eq!(ledger.mtp(15), Some(window[5]));
        assert_eq!(ledger.mtp(25), None);
        ledger.times.remove(&10);
        assert_eq!(ledger.mtp(15), None);
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

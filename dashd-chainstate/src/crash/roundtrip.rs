//! `connect_disconnect_roundtrip` (CH3 §7.1, D48; CH3a a5): a random small regtest block over a
//! random UTXO set, connected by the real `ConnectBlock` and disconnected by the real
//! `DisconnectBlock` ([`SerialCore`] with `NoEvo`'s evo side, as core mode's
//! [`RealConnect`](super::real::RealConnect) drives it). It shares the CRASH model's regtest
//! blocks and in-memory world, not its sweeps.
//!
//! Between the two, random damage to the coins or to the undo record makes the disconnect unclean
//! or failed. A reference of Core's `DisconnectBlock` and `ApplyTxInUndo`
//! (`validation.cpp:2252-2378`) over a map of the coins judges it: the result's class, and, for a
//! result other than `DISCONNECT_FAILED`, every coin the block or the damage touched, byte for
//! byte (a failed disconnect's view is dropped). Without damage the disconnect is clean and leaves
//! the set the block connected on. The connect is checked on the way: its coins, and its undo
//! record, which must be Core's serialization of the coins the block spent.
//!
//! The block sits at [`HEIGHT`] on synthetic ancestors, as `connect_block_micro`'s blocks do: past
//! `COINBASE_MATURITY`, below regtest's masternode payments and DIP0003, with every soft fork
//! below DIP0003 active. Its coins need no signature: `OP_TRUE`, a push dropped before `OP_TRUE`,
//! or P2SH of `OP_TRUE`. Its transactions spend the set's coins and the outputs of the
//! transactions before them, so the disconnect's reverse orders matter.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use dash_chain::{
    BlockIndexRecord, ChainConfig as HeaderConfig, HeaderChain, LoadError, NoStore, Status,
};
use dash_consensus::coins::compress::is_unspendable;
use dash_consensus::coins::{BlockUndo, Coin, CoinsView as _, TxUndo};
use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_consensus::hash::hash160;
use dash_consensus::primitives::{
    Block, BlockHash, BlockHeader, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn, TxOut,
    TxType,
};
use dash_consensus::{Amount, BlockId};
use dashd_store::MemoryEngine;
use dashd_store::faulty::{FaultyFlatFiles, World, WorldConfig};
use dashd_store::flatfile::{
    BlockFiles, BlockFilesConfig, BlockFilesState, FlatFs as _, UNDO_CHECKSUM_SIZE, undo_checksum,
};

use super::real::{CACHES, POOL, cx, services};
use super::synth::{COINBASE_MATURITY, OP_TRUE, REGTEST_BITS, T0, height_push, mine, regtest};
use super::{BLOCKS_DIR, MAGIC};
use crate::chain::{
    AbortPath, BlockStatusWriter, ConnectOutcome, DisconnectResult, RecordingAbort,
};
use crate::checkqueue::CheckQueue;
use crate::coins::{
    BlockOverlay, BlockRef, CoinSrc, CoinsBudget, CoinsCache, OutPointKey, Ratio, Salt,
};
use crate::connect::bench::NoEvoSteps;
use crate::connect::{NodeHelper, SerialCore};
use crate::store::{CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, RecordingSink};

/// The block's height: coinbase coins up to `HEIGHT - 100` are mature; regtest's masternode
/// payments start at 240 and DIP0003 at 432.
pub const HEIGHT: u32 = 120;
/// The block's and its ancestors' version: no BIP9 signal.
const VERSION: i32 = 0x2000_0000;
const OP_DROP: u8 = 0x75;
const OP_RETURN: u8 = 0x6a;
/// The rev file the block's undo record goes to: the first.
const REV: &str = "/dd/blocks/rev00000.dat";

/// The coins by outpoint: the reference's view.
type Set = BTreeMap<OutPointKey, Coin>;

/// Each coin of a list of outpoints, as Core serializes it.
type Coins = Vec<Option<Vec<u8>>>;

/// `data` through the round trip: `Err` names what disagreed with the reference.
///
/// # Errors
///
/// The disagreement, or a step of the rig that failed.
pub fn connect_disconnect_roundtrip(data: &[u8]) -> Result<(), &'static str> {
    run(data).map(|_| ())
}

/// The round trip of `data`, and the disconnect's result.
fn run(data: &[u8]) -> Result<DisconnectResult, &'static str> {
    let mut input = Input(data);
    let before = utxos(&mut input);
    let block = block_of(transactions(&mut input, &before));
    let (mut set, written) = connected(&before, &block);
    let keys: BTreeSet<OutPointKey> = before
        .keys()
        .copied()
        .chain(block.txs().iter().flat_map(|tx| outputs(tx).map(|(k, _)| k)))
        .collect();

    let mut rig = Rig::new(&before, block)?;
    rig.connect()?;
    if rig.coins(&keys)? != expected(&set, &keys) {
        return Err("the coins after the connect");
    }
    let at = rig.undo_at()?;
    let record = [written.core_bytes().as_slice(), &rig.checksum(&written)[..]].concat();
    let mut on_disk = vec![0; record.len()];
    rig.fs.read_exact_at(Path::new(REV), at, &mut on_disk).map_err(|_| "reading the record")?;
    if on_disk != record {
        return Err("the connect's undo record");
    }

    let mut undo = Some(written);
    let damages = input.below(4);
    for _ in 0..damages {
        rig.damage(&mut input, &before, at, &mut set, &mut undo)?;
    }
    let want = disconnect(&mut set, &rig.block, undo.as_ref());
    if damages == 0 && (want != DisconnectResult::Ok || set != before) {
        return Err("the reference's round trip");
    }
    let (got, coins) = rig.disconnect(&keys)?;
    if got != want {
        return Err("the disconnect's result");
    }
    if want != DisconnectResult::Failed && coins != expected(&set, &keys) {
        return Err("the coins after the disconnect");
    }
    if !rig.abort.messages().is_empty() {
        return Err("an AbortNode report");
    }
    Ok(got)
}

/// The input, read a byte at a time; past its end every byte is 0.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn byte(&mut self) -> u8 {
        let Some((&b, rest)) = self.0.split_first() else {
            return 0;
        };
        self.0 = rest;
        b
    }

    /// A number below `n` (at least 1).
    fn below(&mut self, n: usize) -> usize {
        usize::from(self.byte()) % n
    }

    fn value(&mut self) -> Amount {
        Amount::from(u16::from_le_bytes([self.byte(), self.byte()])) * 1_000 + 1
    }
}

/// A lock spent with no signature.
fn lock(input: &mut Input<'_>) -> Vec<u8> {
    match input.below(3) {
        0 => vec![OP_TRUE],
        1 => {
            let n = input.below(41);
            let mut s = vec![n as u8];
            s.extend((0..n).map(|_| input.byte()));
            s.extend([OP_DROP, OP_TRUE]);
            s
        }
        _ => p2sh_op_true(),
    }
}

fn p2sh_op_true() -> Vec<u8> {
    let mut s = vec![0xa9, 0x14];
    s.extend(hash160(&[OP_TRUE]));
    s.push(0x87);
    s
}

/// The `scriptSig` that spends `script`: P2SH's redeem script, else nothing.
fn unlock(script: &[u8]) -> Vec<u8> {
    if script == p2sh_op_true() {
        vec![1, OP_TRUE]
    } else {
        Vec::new()
    }
}

/// The set the block connects on: up to 12 coins below [`HEIGHT`], some sharing a transaction.
fn utxos(input: &mut Input<'_>) -> Set {
    let mut set = Set::new();
    for _ in 0..=input.below(12) {
        let op = OutPoint {
            hash: TxHash::from([0xc0 | input.below(6) as u8; 32]),
            index: input.below(4) as u32,
        };
        let script = lock(input);
        let value = input.value();
        let height = 1 + input.below(HEIGHT as usize - 1) as u32;
        let coinbase = input.byte() & 1 == 1 && height <= HEIGHT - COINBASE_MATURITY;
        set.entry(OutPointKey::new(&op)).or_insert(Coin::new(value, &script, height, coinbase));
    }
    set
}

fn transaction(inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs,
        outputs,
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

/// A coinbase claiming the fees of up to four transactions after it, which spend `set`'s coins
/// and the outputs before them.
fn transactions(input: &mut Input<'_>, set: &Set) -> Vec<Arc<Tx>> {
    let mut pool: Vec<(OutPoint, Amount, Vec<u8>)> = set
        .iter()
        .map(|(k, c)| (k.outpoint(), c.value(), c.script_pubkey().as_slice().to_vec()))
        .collect();
    let (mut txs, mut fees) = (Vec::new(), 0);
    for _ in 0..input.below(5) {
        if pool.is_empty() {
            break;
        }
        let mut inputs = Vec::new();
        let mut sum = 0;
        for _ in 0..=input.below(3).min(pool.len() - 1) {
            let (prevout, value, script) = pool.swap_remove(input.below(pool.len()));
            sum += value;
            inputs.push(TxIn {
                prevout,
                script_sig: unlock(&script).into(),
                sequence: u32::MAX,
            });
        }
        let scripts: Vec<Vec<u8>> = (0..=input.below(3))
            .map(|_| {
                if input.below(4) == 0 {
                    vec![OP_RETURN, 0]
                } else {
                    lock(input)
                }
            })
            .collect();
        // The spendable outputs share what the fee leaves; with none, all of it is the fee.
        let spendable = scripts.iter().filter(|s| !is_unspendable(s)).count() as Amount;
        let fee = (Amount::from(input.byte() % 4) * 100).min(sum);
        let each = if spendable == 0 {
            0
        } else {
            (sum - fee) / spendable
        };
        fees += sum - each * spendable;
        let outputs = scripts
            .into_iter()
            .map(|s| TxOut {
                value: if is_unspendable(&s) {
                    0
                } else {
                    each
                },
                script_pubkey: s.into(),
            })
            .collect();
        let tx = transaction(inputs, outputs);
        pool.extend(
            outputs_made(&tx)
                .map(|(k, out)| (k.outpoint(), out.value, out.script_pubkey.as_bytes().to_vec())),
        );
        txs.push(tx);
    }
    let mut script_sig = height_push(HEIGHT);
    script_sig.extend([1, input.byte()]);
    let null = OutPoint {
        hash: TxHash::from([0; 32]),
        index: u32::MAX,
    };
    let coinbase = transaction(
        vec![TxIn {
            prevout: null,
            script_sig: script_sig.into(),
            sequence: u32::MAX,
        }],
        vec![
            TxOut {
                value: 50_000 + fees,
                script_pubkey: vec![OP_TRUE].into(),
            },
            TxOut {
                value: 0,
                script_pubkey: vec![OP_RETURN].into(),
            },
        ],
    );
    std::iter::once(coinbase).chain(txs).collect()
}

/// Every output of `tx`, by outpoint.
fn outputs(tx: &Tx) -> impl Iterator<Item = (OutPointKey, &TxOut)> {
    let txid = tx.txid();
    tx.outputs().iter().enumerate().map(move |(o, out)| {
        let op = OutPoint {
            hash: txid,
            index: o as u32,
        };
        (OutPointKey::new(&op), out)
    })
}

/// The outputs of `tx` that are coins (`AddCoin` skips an unspendable one, `coins.cpp:71`).
fn outputs_made(tx: &Tx) -> impl Iterator<Item = (OutPointKey, &TxOut)> {
    outputs(tx).filter(|(_, out)| !is_unspendable(out.script_pubkey.as_bytes()))
}

/// The coins `block` makes, as the disconnect expects them.
fn made(block: &Block) -> Vec<(OutPointKey, Coin)> {
    let coin = |i: usize, out: &TxOut| Coin::from_output(out, HEIGHT, i == 0);
    let txs = block.txs().iter().enumerate();
    txs.flat_map(|(i, tx)| outputs_made(tx).map(move |(k, out)| (k, coin(i, out)))).collect()
}

/// The ancestor at `k`: a unique hash, which nothing checks against its header.
fn synthetic_hash(k: u32) -> BlockHash {
    let mut b = [0x5b; 32];
    b[..4].copy_from_slice(&k.to_le_bytes());
    BlockHash::from(b)
}

/// The block at [`HEIGHT`] over `txs`.
fn block_of(txs: Vec<Arc<Tx>>) -> Block {
    let header = BlockHeader {
        version: VERSION,
        prev_hash: synthetic_hash(HEIGHT - 1),
        merkle_root: MerkleRoot::from([0; 32]),
        time: T0 + 150 * HEIGHT,
        bits: REGTEST_BITS,
        nonce: 0,
    };
    mine(header, txs)
}

/// `UpdateCoins` of each of `block`'s transactions over `set` (`validation.cpp:1393-1405`): the
/// coins after the block, and the undo record of the coins it spent.
fn connected(set: &Set, block: &Block) -> (Set, BlockUndo) {
    let mut set = set.clone();
    let mut undo = BlockUndo::default();
    for (i, tx) in block.txs().iter().enumerate() {
        if i > 0 {
            let coins = tx
                .inputs()
                .iter()
                .filter_map(|input| set.remove(&OutPointKey::new(&input.prevout)));
            undo.txs.push(TxUndo {
                coins: coins.collect(),
            });
        }
        set.extend(outputs_made(tx).map(|(k, out)| (k, Coin::from_output(out, HEIGHT, i == 0))));
    }
    (set, undo)
}

// Core: validation.cpp:2282-2378 DisconnectBlock; :2252-2278 ApplyTxInUndo
/// Core's `DisconnectBlock` of `block` over `set` with the undo record read (`None`: the read
/// failed). No generated entry has height 0, so `ApplyTxInUndo`'s legacy path is not taken.
fn disconnect(set: &mut Set, block: &Block, undo: Option<&BlockUndo>) -> DisconnectResult {
    // v:2299-2302 the undo read
    let Some(undo) = undo else {
        return DisconnectResult::Failed;
    };
    // v:2304-2307 "block and undo data inconsistent"
    if undo.txs.len() + 1 != block.txs().len() {
        return DisconnectResult::Failed;
    }
    let mut clean = true;
    // v:2325 the transactions in reverse order
    for (i, tx) in block.txs().iter().enumerate().rev() {
        // v:2333-2344 each output spent and compared with the block's
        for (k, out) in outputs_made(tx) {
            clean &= set.remove(&k) == Some(Coin::from_output(out, HEIGHT, i == 0));
        }
        let Some(txundo) = i.checked_sub(1).map(|u| &undo.txs[u]) else {
            continue;
        };
        // v:2349-2352 "transaction and undo data inconsistent"
        if txundo.coins.len() != tx.inputs().len() {
            return DisconnectResult::Failed;
        }
        // v:2353-2359 the inputs in reverse order
        for (input, coin) in tx.inputs().iter().zip(&txundo.coins).rev() {
            let k = OutPointKey::new(&input.prevout);
            // v:2256 an unspent coin overwritten
            clean &= !set.contains_key(&k);
            // coins.cpp:72 AddCoin skips an unspendable output
            if !is_unspendable(coin.script_pubkey().as_slice()) {
                set.insert(k, coin.clone());
            }
        }
    }
    if clean {
        DisconnectResult::Ok
    } else {
        DisconnectResult::Unclean
    }
}

fn expected(set: &Set, keys: &BTreeSet<OutPointKey>) -> Coins {
    keys.iter().map(|k| set.get(k).map(Coin::serialize)).collect()
}

/// `c` with one of its fields changed, perhaps to its own value; never height 0.
fn changed(input: &mut Input<'_>, c: &Coin) -> Coin {
    let (value, script, height, coinbase) =
        (c.value(), c.script_pubkey().as_slice().to_vec(), c.height(), c.is_coinbase());
    match input.below(4) {
        0 => Coin::new(input.value(), &script, height, coinbase),
        1 => Coin::new(value, &lock(input), height, coinbase),
        2 => Coin::new(value, &script, 1 + input.below(HEIGHT as usize) as u32, coinbase),
        _ => Coin::new(value, &script, height, !coinbase),
    }
}

/// Another undo record: one with a count off, or with a coin changed (perhaps unspendable).
fn rewrite(input: &mut Input<'_>, undo: &mut BlockUndo) {
    let n = undo.txs.len();
    let tx = (n > 0).then(|| input.below(n));
    match (input.below(5), tx) {
        (0, Some(_)) => {
            undo.txs.pop();
        }
        (1, Some(t)) => {
            undo.txs[t].coins.pop();
        }
        (2, Some(t)) if !undo.txs[t].coins.is_empty() => {
            let coins = &mut undo.txs[t].coins;
            let c = input.below(coins.len());
            coins[c] = match input.below(5) {
                // Unspendable, which `AddCoin` skips (`coins.cpp:72`).
                0 => {
                    let old = &coins[c];
                    Coin::new(old.value(), &[OP_RETURN], old.height(), old.is_coinbase())
                }
                _ => changed(input, &coins[c]),
            };
        }
        (3, Some(t)) => {
            let coin = Coin::new(input.value(), &[OP_TRUE], 1, false);
            undo.txs[t].coins.push(coin);
        }
        _ => undo.txs.push(TxUndo::default()),
    }
}

/// The real core, its stores, and the block's index.
struct Rig {
    fs: Arc<FaultyFlatFiles>,
    files: GuardedFiles<FaultyFlatFiles>,
    coins: CoinsCache<MemoryEngine>,
    core: SerialCore,
    abort: Arc<RecordingAbort>,
    chain: HeaderChain,
    block: Block,
    id: BlockId,
}

impl Rig {
    /// The core at `block`'s parent, whose coins are `set`.
    fn new(set: &Set, block: Block) -> Result<Rig, &'static str> {
        let world = World::new(WorldConfig {
            d1: false,
            ..WorldConfig::default()
        });
        let fs = world.fs();
        fs.create_dir_all(Path::new(BLOCKS_DIR)).map_err(|_| "the blocks directory")?;
        let latch = GlobalFailStop::new();
        let cfg = BlockFilesConfig::new(BLOCKS_DIR, MAGIC);
        let files = BlockFiles::with_fs(fs.clone(), cfg, BlockFilesState::default());
        let files = GuardedFiles::new(files, latch.clone());
        let db = GuardedEngine::new(
            MemoryEngine::new(&CHAINSTATE_SCHEMA),
            latch.clone(),
            Arc::new(RecordingSink::default()),
        );
        let budget = CoinsBudget::new(64 << 20, 0, Ratio::HALF, 0);
        let mut coins = CoinsCache::new(Arc::new(db), budget, Salt::fixed(3), None);
        for (k, c) in set {
            coins.add(k, CoinSrc::Bytes(c.as_ref()), false).map_err(|_| "a seeded coin")?;
        }
        let chain = index(&block)?;
        let reader = chain.reader();
        let id = reader.lookup(&block.hash()).ok_or("the block is indexed")?;
        let parent = synthetic_hash(HEIGHT - 1);
        coins.set_best_block(Some(BlockRef {
            id: reader.lookup(&parent).ok_or("its parent is indexed")?,
            hash: parent,
            height: HEIGHT - 1,
        }));
        let abort = Arc::new(RecordingAbort::default());
        let path = AbortPath::new(abort.clone(), latch);
        let queue = Arc::new(CheckQueue::serial());
        let core =
            SerialCore::new(&regtest(), CACHES.clone(), queue, Box::new(NodeHelper), path, None)
                .map_err(|_| "the core")?;
        Ok(Rig {
            fs,
            files,
            coins,
            core,
            abort,
            chain,
            block,
            id,
        })
    }

    fn at(&self) -> BlockRef {
        BlockRef {
            id: self.id,
            hash: self.block.hash(),
            height: HEIGHT,
        }
    }

    /// `ConnectTip`'s `ConnectBlock` of the block read from disk (`fJustCheck` false, `CheckBlock`
    /// again), and the bracket's commit at the block.
    fn connect(&mut self) -> Result<(), &'static str> {
        let params = regtest();
        let reader = self.chain.reader();
        let ancestry = self.chain.complete_ancestry().ok_or("a node chain")?;
        let cache = VersionBitsCache::new();
        let deployments = Deployments::new(&params, &ancestry, &NoEhfSignals, &cache);
        let cx = cx(&params, &reader, &deployments, self.id);
        let at = self.at();
        let mut steps = NoEvoSteps::default();
        let mut overlay = BlockOverlay::new(&mut self.coins);
        let out = {
            let cell = RefCell::new(&mut self.chain);
            let mut st = BlockStatusWriter::new(&cell, self.id);
            let (files, block) = (&self.files, &self.block);
            self.core.connect(&mut steps, &cx, &mut overlay, files, block, false, self.id, &mut st)
        };
        match steps.finish(out) {
            Ok(ConnectOutcome::Connected) => overlay.commit(at).map_err(|_| "the connect commits"),
            Ok(_) => Err("the generated block is invalid"),
            Err(_) => Err("a fatal error in the connect"),
        }
    }

    /// `DisconnectTip`'s `DisconnectBlock` of the block, and `keys`' coins in the view after it,
    /// which is dropped.
    fn disconnect(
        &mut self,
        keys: &BTreeSet<OutPointKey>,
    ) -> Result<(DisconnectResult, Coins), &'static str> {
        let params = regtest();
        let reader = self.chain.reader();
        let ancestry = self.chain.complete_ancestry().ok_or("a node chain")?;
        let cache = VersionBitsCache::new();
        let deployments = Deployments::new(&params, &ancestry, &NoEhfSignals, &cache);
        let cx = cx(&params, &reader, &deployments, self.id);
        let mut steps = NoEvoSteps::default();
        let mut overlay = BlockOverlay::new(&mut self.coins);
        let out = self.core.disconnect(
            &mut steps,
            &cx,
            &mut overlay,
            &self.files,
            &self.block,
            self.id,
            true,
        );
        let out = out.map_err(|_| "a fatal error in the disconnect")?;
        let coins = keys
            .iter()
            .map(|k| {
                let coin = overlay.coin(&k.outpoint()).map_err(|_| "a coins read")?;
                Ok(coin.map(|c| c.to_coin().serialize()))
            })
            .collect::<Result<_, &'static str>>()?;
        Ok((out, coins))
    }

    /// `keys`' coins in the cache.
    fn coins(&mut self, keys: &BTreeSet<OutPointKey>) -> Result<Coins, &'static str> {
        let view = BlockOverlay::new(&mut self.coins);
        keys.iter()
            .map(|k| {
                let coin = view.coin(&k.outpoint()).map_err(|_| "a coins read")?;
                Ok(coin.map(|c| c.to_coin().serialize()))
            })
            .collect()
    }

    /// The undo record's position in [`REV`], which the connect wrote.
    fn undo_at(&self) -> Result<u64, &'static str> {
        let e = self.chain.reader().entry(self.id).ok_or("the block's entry")?;
        if e.file != Some(0) || !e.status.has(Status::HAVE_UNDO) {
            return Err("the connect's undo position");
        }
        Ok(u64::from(e.undo_pos))
    }

    /// The checksum after `undo`'s record: over the parent's hash and the record.
    fn checksum(&self, undo: &BlockUndo) -> [u8; 32] {
        undo_checksum(synthetic_hash(HEIGHT - 1).as_bytes(), undo.core_bytes().as_slice())
    }

    /// One damage to the connected state, which the reference's coins (`set`) and undo record
    /// (`undo`, `None` once unreadable; at `pos` in the rev file) take too.
    fn damage(
        &mut self,
        input: &mut Input<'_>,
        before: &Set,
        pos: u64,
        set: &mut Set,
        undo: &mut Option<BlockUndo>,
    ) -> Result<(), &'static str> {
        let rev = Path::new(REV);
        let made = made(&self.block);
        let spent: Vec<(&OutPointKey, &Coin)> =
            before.iter().filter(|(k, _)| !set.contains_key(k)).collect();
        let mut coin = None;
        match input.below(6) {
            // An output the block made is missing: `SpendCoin` finds nothing.
            0 => {
                let (k, _) = &made[input.below(made.len())];
                let at = self.at();
                let mut overlay = BlockOverlay::new(&mut self.coins);
                overlay.spend(k).map_err(|_| "a damaging spend")?;
                overlay.commit(at).map_err(|_| "the damage commits")?;
                set.remove(k);
            }
            // An output the block made differs from the block's.
            1 => {
                let (k, c) = &made[input.below(made.len())];
                coin = Some((*k, changed(input, c)));
            }
            // A coin the block spent is unspent again, perhaps changed.
            2 if !spent.is_empty() => {
                let (k, c) = spent[input.below(spent.len())];
                let c = if input.byte() & 1 == 0 {
                    c.clone()
                } else {
                    changed(input, c)
                };
                coin = Some((*k, c));
            }
            _ if undo.is_none() => {}
            // A byte of the record or its checksum flipped: the read fails.
            3 => {
                let len = undo.as_ref().ok_or("a readable record")?.core_bytes().as_slice().len();
                let span = (len + UNDO_CHECKSUM_SIZE as usize) as u64;
                let off = pos + u64::from(u16::from_le_bytes([input.byte(), input.byte()])) % span;
                let mut b = [0];
                self.fs.read_exact_at(rev, off, &mut b).map_err(|_| "reading the rev file")?;
                let flipped = [b[0] ^ (1 << input.below(8))];
                self.fs.write_all_at(rev, off, &flipped).map_err(|_| "damaging the rev file")?;
                *undo = None;
            }
            // The rev file is gone.
            4 => {
                self.fs.remove(rev).map_err(|_| "removing the rev file")?;
                *undo = None;
            }
            // Another record, checksummed.
            _ => {
                let u = undo.as_mut().ok_or("a readable record")?;
                rewrite(input, u);
                let record = [u.core_bytes().as_slice(), &self.checksum(u)[..]].concat();
                self.fs.write_all_at(rev, pos, &record).map_err(|_| "rewriting the record")?;
            }
        }
        if let Some((k, c)) = coin {
            let at = self.at();
            let mut overlay = BlockOverlay::new(&mut self.coins);
            overlay.add(&k, CoinSrc::Bytes(c.as_ref()), true).map_err(|_| "a damaging add")?;
            overlay.commit(at).map_err(|_| "the damage commits")?;
            set.insert(k, c);
        }
        Ok(())
    }
}

/// The header chain: regtest's genesis, synthetic ancestors (connected, with data and undo), and
/// `block` at [`HEIGHT`], accepted.
fn index(block: &Block) -> Result<HeaderChain, &'static str> {
    let params = regtest();
    let svc = services(&params);
    let cfg = HeaderConfig::node(&params);
    let mut genesis: HeaderChain =
        HeaderChain::new_genesis(params.clone(), cfg.clone(), svc.clone(), NoStore)
            .map_err(|_| "regtest's genesis")?;
    let g = genesis.node_index().map_err(|_| "a node index")?.take_dirty().pop();
    let g = g.ok_or("the genesis record")?;
    let hash_at = |k: u32| match k {
        0 => g.hash,
        k if k == HEIGHT => block.hash(),
        k => synthetic_hash(k),
    };
    let records = (1..=HEIGHT).map(|k| {
        let record = if k < HEIGHT {
            BlockIndexRecord {
                height: k,
                status: Status(Status::VALID_SCRIPTS | Status::HAVE_DATA | Status::HAVE_UNDO),
                n_tx: 1,
                file: Some(0),
                data_pos: Some(8),
                undo_pos: Some(8),
                header: BlockHeader {
                    version: VERSION,
                    prev_hash: hash_at(k - 1),
                    merkle_root: MerkleRoot::from([0; 32]),
                    time: T0 + 150 * k,
                    bits: REGTEST_BITS,
                    nonce: k,
                },
            }
        } else {
            BlockIndexRecord {
                height: k,
                status: Status(Status::VALID_TRANSACTIONS | Status::HAVE_DATA),
                n_tx: block.txs().len() as u32,
                file: Some(0),
                data_pos: Some(8),
                undo_pos: None,
                header: *block.header().raw(),
            }
        };
        Ok::<_, LoadError>((hash_at(k), record.to_bytes()))
    });
    let records = std::iter::once(Ok((g.hash, g.bytes))).chain(records);
    HeaderChain::load(params, cfg, svc, records, &POOL).map_err(|_| "the index loads")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Random inputs: each passes, and each of the three results occurs.
    #[test]
    fn random_inputs_round_trip_as_cores_rules_say() {
        let mut s: u64 = 0x2545_f491_4f6c_dd1d;
        let mut seen = BTreeMap::<_, usize>::new();
        for _ in 0..300 {
            let data: Vec<u8> = (0..160)
                .map(|_| {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    s as u8
                })
                .collect();
            match run(&data) {
                Ok(r) => *seen.entry(format!("{r:?}")).or_default() += 1,
                Err(e) => {
                    panic!("{e}: {}", data.iter().map(|b| format!("{b:02x}")).collect::<String>())
                }
            }
        }
        assert_eq!(seen.len(), 3, "{seen:?}");
    }
}

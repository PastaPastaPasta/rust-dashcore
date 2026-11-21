//! MP1a-5's binding tests: each pins the Core v24.0.0 lines it cites (`v:` is validation.cpp,
//! `tm:` txmempool.cpp).

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::sync::Arc;

use dash_consensus::coins::Coin;
use dash_consensus::ctx::{ChainAncestry, CheckpointView, Deployment, DeploymentView};
use dash_consensus::hash::sha256d;
use dash_consensus::script::ScriptPolicy;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_consensus::{BlockId, ConsensusCtx, Tx, TxRef};
use dash_dml::MnList;
use dash_dml::StoreError;
use dash_dml::blockprocessor::{BlockLookup, QcBlockError};
use dash_dml::creditpool::{CreditPool, PoolError, RangesSet};
use dash_dml::netinfo::NetInfoRules;
use dash_dml::specialtx::qc::CommitmentVerifier;
use dash_dml::specialtx::quorum::{QuorumKey, QuorumLookup, QuorumScan, SeamError};
use dash_dml::specialtx::reasons::{
    BAD_ASSETUNLOCK_NOT_VERIFIED, BAD_ASSETUNLOCK_TOO_LATE, BAD_ASSETUNLOCK_TOO_OLD_QUORUM,
};
use dash_dml::specialtx::sig::SerialVerifier;
use dash_dml::specialtx::{self, QuorumSeams, SpecialTxCtx, check_special_tx_inner};
use dash_dml::testkit::replay::{CORE_NET_INFO, mainnet};
use dash_dml::testkit::{MemChain, operator_key, operator_sig};
use dash_evo::ctx::EvoCtx;
use dash_evo::effect::EffectSink;
use dash_evo::llmq::{CompleteChain, LlmqCommitment, LlmqCtx};
use dash_evo::locks::sign_hash;
use dash_params::{ChainSetup, LlmqType};
use dash_pkc::bls::{BlsCtx, BlsGlobalSig, BlsSchemeKind};
use dash_primitives::{
    AssetUnlock, BlockHash, OutPoint, QuorumHash, Transaction, TxHash, TxIn, TxOut, TxType,
};
use dash_types::codec::BaseCodec;
use dashd_chainstate::chain::seams::AtmpCoins;
use dashd_chainstate::chain::{Fatal, ReorgCx, TipFacts};
use dashd_chainstate::checkqueue::script::{ScriptOptions, TxSlot, exec_cache_probe};
use dashd_chainstate::connect::SpecialTxRules;
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches};

use super::consensus::{
    calculate_lock_points_at_tip, calculate_prev_heights, check_final_tx_at_tip,
    check_sequence_locks_at_tip, check_tx_inputs_at_tip, consensus_script_checks,
    policy_script_checks,
};
use super::reorg_filter::filter_final_and_mature;
use super::special::{
    SpecialTxChain, check_special_tx_at_tip, check_special_tx_for_mempool,
    check_unlock_index_unmined,
};
use super::view::{InputView, MEMPOOL_HEIGHT, fetch_inputs, mempool_view_coin};
use crate::api::MempoolRead;
use crate::options::MemPoolOptions;
use crate::policy::consts::STANDARD_SCRIPT_VERIFY_FLAGS;
use crate::pool::entry::{EntryMetadata, PoolEntry};
use crate::types::LockPoints;

const OP_TRUE: u8 = 0x51;
const OP_FALSE: u8 = 0x00;
const OP_NOP1: u8 = 0xb0;
const OP_CHECKMULTISIG: u8 = 0xae;
const DISABLE: u32 = 1 << 31;

pub(super) fn params() -> &'static ChainSetup {
    mainnet()
}

pub(super) fn outpoint(byte: u8, n: u32) -> OutPoint {
    OutPoint::new(TxHash::from([byte; 32]), n)
}

/// A transaction of `version` spending `inputs` (`(outpoint, nSequence)`, empty scriptSigs), with
/// one `OP_TRUE` output per value.
fn spend(version: i16, inputs: &[(OutPoint, u32)], values: &[i64], lock_time: u32) -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        version,
        TxType::Spend,
        inputs.iter().map(|&(prevout, seq)| TxIn::new(prevout, vec![].into(), seq)).collect(),
        values.iter().map(|&v| TxOut::from_duffs(v, vec![OP_TRUE].into())).collect(),
        lock_time,
        vec![],
    )))
}

/// The pool's `get` over a fixed set; nothing else is read.
#[derive(Default)]
struct Pool(BTreeMap<TxHash, TxRef>);

impl Pool {
    fn with(txs: &[&TxRef]) -> Pool {
        Pool(txs.iter().map(|tx| (tx.txid(), (*tx).clone())).collect())
    }
}

impl MempoolRead for Pool {
    fn get(&self, txid: &TxHash) -> Option<TxRef> {
        self.0.get(txid).cloned()
    }
    fn spender_of(&self, _: &OutPoint) -> Option<TxRef> {
        unreachable!("the binding reads no spender")
    }
    fn asset_unlock_txids_by_index(&self, _: u64) -> Vec<TxHash> {
        unreachable!("the binding reads no unlock index")
    }
    fn pending_asset_unlock_amount(&self) -> i64 {
        unreachable!("the binding reads no pending total")
    }
    fn service_updates_before_key_change(&self, _: &TxHash, _: &MnList) -> Vec<TxHash> {
        unreachable!("the binding reads no ProTx map")
    }
    fn is_unorderable_service_update(&self, _: &Tx, _: &[TxHash], _: &MnList) -> bool {
        unreachable!("the binding reads no ProTx map")
    }
    fn key_change_target(&self, _: &Tx, _: &MnList) -> Option<dash_dml::ProTxHash> {
        unreachable!("the binding reads no ProTx map")
    }
}

/// CoinsTip as `AtmpCoins`: a caching read over `coins`, which records what it cached, and a
/// fault on any outpoint in `faults`.
pub(super) struct TipCoins {
    pub(super) coins: BTreeMap<OutPoint, Coin>,
    pub(super) cached: BTreeSet<OutPoint>,
    best: Option<BlockHash>,
    faults: BTreeSet<OutPoint>,
}

impl TipCoins {
    pub(super) fn new(best: BlockHash, coins: &[(OutPoint, Coin)]) -> TipCoins {
        TipCoins {
            coins: coins.iter().cloned().collect(),
            cached: BTreeSet::new(),
            best: Some(best),
            faults: BTreeSet::new(),
        }
    }
}

impl AtmpCoins for TipCoins {
    fn fetch(&mut self, outpoint: &OutPoint) -> Result<Option<Coin>, Fatal> {
        if self.faults.contains(outpoint) {
            return Err(Fatal::new("injected coins read fault"));
        }
        let coin = self.coins.get(outpoint).cloned();
        if coin.is_some() {
            self.cached.insert(*outpoint);
        }
        Ok(coin)
    }
    fn have_in_cache(&self, outpoint: &OutPoint) -> bool {
        self.cached.contains(outpoint)
    }
    fn uncache(&mut self, outpoint: &OutPoint) {
        self.cached.remove(outpoint);
    }
    fn best_block(&self) -> Option<BlockHash> {
        self.best
    }
}

/// A block tree and its active tip, with CoinsTip for the reorg filter.
struct Chain {
    chain: MemChain,
    tip: BlockId,
    coins: TipCoins,
}

impl Chain {
    /// A chain of `height` blocks over genesis; MemChain's ids are the heights.
    fn new(height: u32) -> Chain {
        let mut chain = MemChain::new();
        let tip = chain.extend(chain.genesis(), height);
        let best = chain.hash(tip);
        Chain {
            chain,
            tip,
            coins: TipCoins::new(best, &[]),
        }
    }

    fn at(&self, height: u32) -> BlockId {
        self.chain.ancestor(self.tip, height).expect("on the active chain")
    }

    fn with_coin(mut self, outpoint: OutPoint, coin: Coin) -> Chain {
        self.coins.coins.insert(outpoint, coin);
        self
    }
}

impl TipFacts for Chain {
    fn tip(&self) -> Option<BlockId> {
        Some(self.tip)
    }
    fn ancestry(&self) -> &dyn ChainAncestry {
        &self.chain
    }
}

impl ReorgCx for Chain {
    fn chain_contains(&self, block: BlockId) -> bool {
        self.chain.ancestor(self.tip, self.chain.header(block).height) == Some(block)
    }
    fn access_coin(&mut self, outpoint: &OutPoint) -> Result<Option<Coin>, Fatal> {
        self.coins.fetch(outpoint)
    }
}

fn coin(height: u32, coinbase: bool) -> Coin {
    Coin::new(10_000, &[OP_TRUE], height, coinbase)
}

fn view_of(tx: &TxRef, pool: &Pool, chain: &mut Chain) -> (InputView, Vec<OutPoint>) {
    let mut uncache = Vec::new();
    let tip = chain.chain.hash(chain.tip);
    let view = fetch_inputs(tx, pool, &mut chain.coins, tip, &mut uncache)
        .expect("no fault")
        .expect("inputs present");
    (view, uncache)
}

fn view_heights(tx: &Tx, view: &InputView) -> Vec<u32> {
    calculate_prev_heights(tx, &mut |o| Ok(view.get(o).map(Coin::height)))
        .expect("no fault")
        .expect("all inputs")
}

fn reject(result: TxValidationResult, reason: &str) -> TxValidationError {
    TxValidationError::new(result, reason.to_owned())
}

/// `CCoinsViewMemPool::GetCoin` (`tm:1954-1975`) answers an in-pool parent's output at
/// MEMPOOL_HEIGHT, with no fallback past its outputs; `CalculatePrevHeights` (`v:244-257`) counts
/// it at tip + 1, and `CalculateLockPointsAtTip` (`v:280-306`) leaves it out of `maxInputBlock`.
/// `CheckSequenceLocksAtTip` (`v:308-324`) then admits a zero lock on it and refuses a one-block
/// lock (`non-BIP68-final`, `v:1010-1012`).
#[test]
fn lock_points_count_pool_coins_at_tip_plus_one() {
    let parent = spend(2, &[(outpoint(1, 0), u32::MAX)], &[5_000], 0);
    let pool = Pool::with(&[&parent]);
    let confirmed = outpoint(2, 0);
    let mut chain = Chain::new(20).with_coin(confirmed, coin(10, false));

    assert_eq!(
        mempool_view_coin(&pool, &OutPoint::new(parent.txid(), 1), &mut |_| panic!("no base")),
        Ok(None)
    );

    let child = spend(2, &[(OutPoint::new(parent.txid(), 0), 0), (confirmed, 5)], &[1_000], 0);
    let (view, uncache) = view_of(&child, &pool, &mut chain);
    assert_eq!(uncache, vec![OutPoint::new(parent.txid(), 0), confirmed]);
    let pool_coin = view.get(&OutPoint::new(parent.txid(), 0)).expect("cached");
    assert_eq!((pool_coin.height(), pool_coin.is_coinbase()), (MEMPOOL_HEIGHT, false));

    let heights = view_heights(&child, &view);
    assert_eq!(heights, vec![MEMPOOL_HEIGHT, 10]);
    let lp = calculate_lock_points_at_tip(params(), &chain, &child, heights).expect("no fault");
    // The pool input: 21 + 0 - 1; the confirmed one: 10 + 5 - 1.
    assert_eq!(
        lp,
        LockPoints {
            height: 20,
            time: -1,
            max_input_block: Some(chain.at(10)),
        }
    );
    assert_eq!(check_sequence_locks_at_tip(params(), &chain, &lp), Ok(true));

    let locked = spend(2, &[(OutPoint::new(parent.txid(), 0), 1)], &[1_000], 0);
    let (view, _) = view_of(&locked, &pool, &mut chain);
    let lp = calculate_lock_points_at_tip(params(), &chain, &locked, view_heights(&locked, &view))
        .expect("no fault");
    // Only a mempool input: maxInputBlock is genesis.
    assert_eq!((lp.height, lp.max_input_block), (21, Some(chain.at(0))));
    assert_eq!(check_sequence_locks_at_tip(params(), &chain, &lp), Ok(false));
}

/// `PreChecks`' input loop (`v:972-990`): a missing input is `txn-already-known` when CoinsTip
/// holds one of the transaction's own outputs, else `bad-txns-inputs-missingorspent`; every
/// outpoint CoinsTip had not cached is listed for uncaching. The best-block pin (`v:994-1003`)
/// asserts CoinsTip's best block is the tip.
#[test]
fn input_loop_outcomes_and_best_block_pin() {
    let present = outpoint(3, 0);
    let mut chain = Chain::new(5).with_coin(present, coin(1, false));
    let tip = chain.chain.hash(chain.tip);
    let tx = spend(2, &[(present, u32::MAX), (outpoint(4, 0), u32::MAX)], &[1_000], 0);
    let mut uncache = Vec::new();
    let got = fetch_inputs(&tx, &Pool::default(), &mut chain.coins, tip, &mut uncache);
    assert_eq!(
        got.expect("no fault").err(),
        Some(reject(TxValidationResult::MissingInputs, "bad-txns-inputs-missingorspent"))
    );
    assert_eq!(uncache, vec![present, outpoint(4, 0)]);

    chain.coins.cached.insert(OutPoint::new(tx.txid(), 0));
    let got = fetch_inputs(&tx, &Pool::default(), &mut chain.coins, tip, &mut Vec::new());
    assert_eq!(
        got.expect("no fault").err(),
        Some(reject(TxValidationResult::Conflict, "txn-already-known"))
    );

    let single = spend(2, &[(present, u32::MAX)], &[1_000], 0);
    let other = chain.chain.hash(chain.at(4));
    let got = fetch_inputs(&single, &Pool::default(), &mut chain.coins, other, &mut Vec::new());
    assert!(got.is_err(), "the best-block assert");

    chain.coins.faults.insert(present);
    chain.coins.cached.clear();
    let got = fetch_inputs(&single, &Pool::default(), &mut chain.coins, tip, &mut Vec::new());
    assert_eq!(got.err(), Some(Fatal::new("injected coins read fault")));
}

/// `Consensus::CheckTxInputs` at the tip's height plus one (`v:1015-1018`;
/// `consensus/tx_verify.cpp:163-190`): a coinbase output 99 blocks deep at the next block is
/// `bad-txns-premature-spend-of-coinbase`, 100 deep is spendable.
#[test]
fn coinbase_maturity_rejects_at_tip_plus_one() {
    let coinbase = outpoint(5, 0);
    let tx = spend(2, &[(coinbase, u32::MAX)], &[1_000], 0);
    for (tip_height, verdict) in [
        (
            108,
            Err(reject(TxValidationResult::PrematureSpend, "bad-txns-premature-spend-of-coinbase")),
        ),
        (109, Ok(9_000)),
    ] {
        let mut chain = Chain::new(tip_height).with_coin(coinbase, coin(10, true));
        let (view, _) = view_of(&tx, &Pool::default(), &mut chain);
        let got = check_tx_inputs_at_tip(&tx, &view, tip_height)
            .map_err(|e| reject(e.result(), e.reject_reason()));
        assert_eq!(got, verdict, "tip {tip_height}");
    }
}

/// A transaction spending one confirmed coin per script, with `script_sig` pushed on each.
fn scripted(scripts: &[&[u8]], script_sig: &[u8]) -> (TxRef, Chain, InputView) {
    let inputs: Vec<_> = (0..scripts.len()).map(|i| (outpoint(6, i as u32), u32::MAX)).collect();
    let tx = Arc::new(Tx::new(Transaction::new(
        2,
        TxType::Spend,
        inputs.iter().map(|&(p, s)| TxIn::new(p, script_sig.to_vec().into(), s)).collect(),
        vec![TxOut::from_duffs(1_000, vec![OP_TRUE].into())],
        0,
        vec![],
    )));
    let mut chain = Chain::new(10);
    for (i, script) in scripts.iter().enumerate() {
        chain.coins.coins.insert(outpoint(6, i as u32), Coin::new(10_000, script, 1, false));
    }
    let (view, _) = view_of(&tx, &Pool::default(), &mut chain);
    (tx, chain, view)
}

fn policy(scripts: &[&[u8]], script_sig: &[u8]) -> Result<(), TxValidationError> {
    let (tx, _, view) = scripted(scripts, script_sig);
    let slot = TxSlot::new(&tx);
    policy_script_checks(&slot, &view, None).expect("no fault")
}

/// `PolicyScriptChecks` (`v:1186-1202`) through `CheckInputScripts`' serial loop
/// (`v:2193-2234`): v24's classification (MPIX-01). A failure the mandatory flags alone also
/// raise (`OP_FALSE`; NULLDUMMY, mandatory from v24) is `mandatory-script-verify-flag-failed`,
/// `TX_CONSENSUS`; one that passes without the non-mandatory flags (an upgradable NOP) is
/// `non-mandatory-script-verify-flag`, `TX_NOT_STANDARD`.
#[test]
fn script_failures_are_classified_under_v24() {
    let nop: &[u8] = &[OP_NOP1, OP_TRUE];
    let false_: &[u8] = &[OP_FALSE];
    let zero_of_zero: &[u8] = &[OP_FALSE, OP_FALSE, OP_CHECKMULTISIG];
    assert_eq!(policy(&[&[OP_TRUE]], &[]), Ok(()));
    assert_eq!(
        policy(&[nop], &[]),
        Err(reject(
            TxValidationResult::NotStandard,
            "non-mandatory-script-verify-flag (NOPx reserved for soft-fork upgrades)"
        ))
    );
    assert_eq!(
        policy(&[false_], &[]),
        Err(reject(
            TxValidationResult::Consensus,
            "mandatory-script-verify-flag-failed (Script evaluated without error but finished with a false/empty top stack element)"
        ))
    );
    // A non-null dummy for CHECKMULTISIG: scriptSig pushes 1.
    assert_eq!(
        policy(&[zero_of_zero], &[OP_TRUE]),
        Err(reject(
            TxValidationResult::Consensus,
            "mandatory-script-verify-flag-failed (Dummy CHECKMULTISIG argument must be zero)"
        ))
    );
}

/// Two failing inputs with different errors and classes: the lowest index's first-run error and
/// its class, whichever order the classes come in (`v:2193-2234`).
#[test]
fn lowest_failing_input_decides_error_and_class() {
    let nop: &[u8] = &[OP_NOP1, OP_TRUE];
    let false_: &[u8] = &[OP_FALSE];
    let non_mandatory = policy(&[nop, false_], &[]).expect_err("fails");
    assert_eq!(
        (non_mandatory.result(), non_mandatory.reject_reason()),
        (
            TxValidationResult::NotStandard,
            "non-mandatory-script-verify-flag (NOPx reserved for soft-fork upgrades)"
        )
    );
    let mandatory = policy(&[false_, nop], &[]).expect_err("fails");
    assert_eq!(mandatory.result(), TxValidationResult::Consensus);
    assert!(mandatory.reject_reason().contains("false/empty top stack element"));
}

fn caches() -> ScriptCaches {
    ScriptCaches::new(CacheSizes::default()).expect("cache sizes")
}

/// The cache settings: `PolicyScriptChecks` stores signatures but not the script execution
/// (`cacheFullScriptStore = false`, `v:1197`); `ConsensusScriptChecks` stores the execution under
/// the tip's block flags (`v:567`, `cacheFullScriptStore = true`), after its coin-equality
/// asserts (`v:544-562`).
#[test]
fn script_execution_cache_is_filled_only_by_consensus_checks() {
    let (tx, mut chain, view) = scripted(&[&[OP_TRUE]], &[]);
    let caches = caches();
    let block_flags = dash_consensus::script::ScriptFlags::P2SH;
    let slot = TxSlot::new(&tx);
    // A script-execution hit is the probe's `None`; probing with the full store on erases nothing.
    let held = |flags| {
        let opts = ScriptOptions {
            flags,
            policy: ScriptPolicy::V24_0,
            caches: Some(&caches),
            cache_sig_store: true,
            cache_full_script_store: true,
        };
        exec_cache_probe(&slot, &opts).is_none()
    };

    assert_eq!(policy_script_checks(&slot, &view, Some(&caches)), Ok(Ok(())));
    assert!(!held(STANDARD_SCRIPT_VERIFY_FLAGS));
    assert!(!held(block_flags));

    let pool = Pool::default();
    let got =
        consensus_script_checks(&slot, &view, &pool, &mut chain.coins, block_flags, Some(&caches));
    assert_eq!(got, Ok(Ok(())));
    assert!(held(block_flags));
    assert!(!held(STANDARD_SCRIPT_VERIFY_FLAGS));
}

/// `CheckInputsFromMempoolAndCache`'s asserts (`v:551-561`): a CoinsTip coin that differs from
/// `m_view`'s is Core's assert, never a verdict; so is an in-pool parent's output.
#[test]
fn consensus_checks_assert_coin_equality() {
    let (tx, mut chain, view) = scripted(&[&[OP_TRUE]], &[]);
    chain.coins.coins.insert(outpoint(6, 0), Coin::new(9_999, &[OP_TRUE], 1, false));
    let slot = TxSlot::new(&tx);
    let got = consensus_script_checks(
        &slot,
        &view,
        &Pool::default(),
        &mut chain.coins,
        STANDARD_SCRIPT_VERIFY_FLAGS,
        None,
    );
    assert!(got.is_err());
}

/// A pool entry of `tx` with `lock_points`, spending a coinbase or not.
fn entry(tx: &TxRef, lock_points: LockPoints, spends_coinbase: bool) -> PoolEntry {
    let metadata = EntryMetadata {
        fee: 1_000,
        lock_points,
        spends_coinbase,
        ..EntryMetadata::default()
    };
    PoolEntry::new(tx.clone(), metadata, &MemPoolOptions::default())
}

/// The lock points of a transaction without relative locks, as `CalculateLockPointsAtTip` gives
/// them (`v:300-305`): no height, no time, `maxInputBlock` genesis.
fn unlocked(chain: &Chain) -> LockPoints {
    LockPoints {
        height: -1,
        time: -1,
        max_input_block: Some(chain.at(0)),
    }
}

fn filter(
    chain: &mut Chain,
    pool: &Pool,
    entry: &PoolEntry,
) -> Result<(bool, Option<LockPoints>), Fatal> {
    filter_final_and_mature(params(), pool, entry, chain)
}

/// `filter_final_and_mature`'s first test, `CheckFinalTxAtTip` (`v:482`, `:205-225`): a
/// height-locked transaction final at the tip's child is removed once a disconnect makes the tip's
/// child its lock height.
#[test]
fn reorg_filter_removes_non_final_after_disconnect() {
    let tx = spend(2, &[(outpoint(7, 0), 0xffff_fffe)], &[1_000], 30);
    let mut chain = Chain::new(30);
    let e = entry(&tx, unlocked(&chain), false);
    assert_eq!(check_final_tx_at_tip(&chain, &tx), Ok(true));
    assert_eq!(filter(&mut chain, &Pool::default(), &e), Ok((false, None)));
    chain.tip = chain.at(29);
    assert_eq!(check_final_tx_at_tip(&chain, &tx), Ok(false));
    assert_eq!(filter(&mut chain, &Pool::default(), &e), Ok((true, None)));
}

/// Still-valid lock points (`TestLockPointValidity`, `tm:40-55`) are checked as they are
/// (`v:488-491`): a lock the tip's child met fails once the tip is disconnected.
#[test]
fn reorg_filter_rechecks_valid_lock_points_at_the_new_tip() {
    let tx = spend(2, &[(outpoint(8, 0), 21)], &[1_000], 0);
    let mut chain = Chain::new(30);
    let lp = LockPoints {
        height: 30,
        time: -1,
        max_input_block: Some(chain.at(10)),
    };
    let e = entry(&tx, lp, false);
    assert_eq!(filter(&mut chain, &Pool::default(), &e), Ok((false, None)));
    chain.tip = chain.at(29);
    assert_eq!(filter(&mut chain, &Pool::default(), &e), Ok((true, None)));
}

/// Lock points whose `maxInputBlock` left the active chain are recalculated over CoinsTip
/// overlaid with the pool (`v:492-500`): the in-pool input counts at the tip's height plus one, the
/// confirmed one at its new height, and the entry gets the new points. A recalculation the next
/// block fails removes the entry, with no update.
#[test]
fn reorg_filter_recalculates_lock_points_off_the_active_chain() {
    let parent = spend(2, &[(outpoint(9, 0), u32::MAX)], &[5_000], 0);
    let pool = Pool::with(&[&parent]);
    let confirmed = outpoint(10, 0);
    let mut chain = Chain::new(30).with_coin(confirmed, coin(22, false));
    let fork = chain.chain.extend(chain.at(20), 12);
    let stale = LockPoints {
        height: 27,
        time: -1,
        max_input_block: chain.chain.ancestor(fork, 25),
    };
    assert!(!ReorgCx::chain_contains(&chain, stale.max_input_block.expect("set")));

    let tx = spend(2, &[(OutPoint::new(parent.txid(), 0), DISABLE), (confirmed, 3)], &[1_000], 0);
    let e = entry(&tx, stale, false);
    let fresh = LockPoints {
        height: 24,
        time: -1,
        max_input_block: Some(chain.at(22)),
    };
    assert_eq!(filter(&mut chain, &pool, &e), Ok((false, Some(fresh))));
    assert_eq!(chain.coins.cached, BTreeSet::from([confirmed]), "CoinsTip read, pool overlaid");

    chain.coins.coins.insert(confirmed, coin(29, false));
    assert_eq!(filter(&mut chain, &pool, &e), Ok((true, None)));
}

/// The coinbase-maturity half (`v:504-515`): an out-of-pool coinbase output immature at the tip's
/// height plus one removes the entry; in-pool inputs are not read.
#[test]
fn reorg_filter_removes_immature_coinbase_spend() {
    let parent = spend(2, &[(outpoint(11, 0), u32::MAX)], &[5_000], 0);
    let pool = Pool::with(&[&parent]);
    let coinbase = outpoint(12, 0);
    let tx =
        spend(2, &[(OutPoint::new(parent.txid(), 0), u32::MAX), (coinbase, u32::MAX)], &[1], 0);
    for (coin_height, remove) in [(52, true), (51, false)] {
        let mut chain = Chain::new(150).with_coin(coinbase, coin(coin_height, true));
        let e = entry(&tx, unlocked(&chain), true);
        chain.coins.faults.insert(OutPoint::new(parent.txid(), 0));
        assert_eq!(filter(&mut chain, &pool, &e), Ok((remove, None)), "coin at {coin_height}");
    }
}

/// A coins read fault in the recalculation or in the maturity read is a fault, never a verdict:
/// the filter returns no result to apply (`ReorgCx::access_coin`'s contract, `coins.cpp:355-369`).
#[test]
fn reorg_filter_coin_faults_are_fatal() {
    let confirmed = outpoint(13, 0);
    let mut chain = Chain::new(30).with_coin(confirmed, coin(5, true));
    let fork = chain.chain.extend(chain.at(20), 12);
    let stale = LockPoints {
        max_input_block: chain.chain.ancestor(fork, 25),
        ..LockPoints::default()
    };
    let tx = spend(2, &[(confirmed, u32::MAX)], &[1_000], 0);
    chain.coins.faults.insert(confirmed);
    let injected = Err(Fatal::new("injected coins read fault"));
    assert_eq!(filter(&mut chain, &Pool::default(), &entry(&tx, stale, false)), injected);
    let mature = entry(&tx, unlocked(&chain), true);
    assert_eq!(filter(&mut chain, &Pool::default(), &mature), injected);

    chain.coins.faults.clear();
    chain.coins.coins.clear();
    let missing = filter(&mut chain, &Pool::default(), &mature);
    assert!(missing.is_err(), "Core asserts the coin is unspent");
}

fn unlock_payload(
    index: u64,
    requested_height: u32,
    quorum: QuorumHash,
    sig: [u8; 96],
) -> AssetUnlock {
    AssetUnlock::new(
        2,
        index,
        1_000,
        requested_height,
        quorum,
        BlsGlobalSig::decode(&sig).expect("decodes"),
    )
}

fn unlock_tx(p: &AssetUnlock) -> TxRef {
    let mut payload = Vec::new();
    p.encode(&mut payload);
    let script = [&[0x76, 0xa9, 0x14][..], &[9; 20], &[0x88, 0xac]].concat();
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::AssetUnlock,
        vec![],
        vec![TxOut::from_duffs(1_000, script.into())],
        0,
        payload,
    )))
}

/// `PreChecks`' mined-index check (`v:925-952`): an index the tip's pool holds is
/// `txn-already-known` (`TX_CONFLICT`); a pool whose construction failed is
/// `failed-getcreditpool` (`TX_BAD_SPECIAL`); without V20 the pool is empty
/// (`evo/creditpool.cpp:124`); other transactions are not checked.
#[test]
fn credit_pool_index_outcomes() {
    let tx = unlock_tx(&unlock_payload(7, 100, QuorumHash::from([1; 32]), [0; 96]));
    let mut indexes = RangesSet::default();
    indexes.add(7);
    let pool = |indexes| -> dash_dml::creditpool::PoolState {
        Ok(CreditPool {
            locked: 0,
            current_limit: 0,
            lately_unlocked: 0,
            indexes,
        })
    };
    assert_eq!(
        check_unlock_index_unmined(&tx, Some(&pool(indexes))),
        Err(reject(TxValidationResult::Conflict, "txn-already-known"))
    );
    assert_eq!(check_unlock_index_unmined(&tx, Some(&pool(RangesSet::default()))), Ok(()));
    assert_eq!(check_unlock_index_unmined(&tx, None), Ok(()));
    assert_eq!(
        check_unlock_index_unmined(&tx, Some(&Err(PoolError::PrevLocked))),
        Err(reject(TxValidationResult::BadSpecial, "failed-getcreditpool"))
    );
    let plain = spend(2, &[(outpoint(14, 0), u32::MAX)], &[1], 0);
    assert_eq!(check_unlock_index_unmined(&plain, Some(&Err(PoolError::PrevLocked))), Ok(()));
}

/// Every deployment active: DIP3 gates the check, V19 picks the basic scheme.
struct AllActive;

impl DeploymentView for AllActive {
    fn active_after(&self, _: Option<BlockId>, _: Deployment) -> bool {
        true
    }
    fn active_at(&self, _: BlockId, _: Deployment) -> bool {
        true
    }
}

impl CheckpointView for AllActive {
    fn highest_checkpoint_in_index(&self) -> Option<u32> {
        None
    }
}

struct NoCommitments;

impl CommitmentVerifier for NoCommitments {
    fn verify_null(&self, _: &LlmqCommitment) -> bool {
        unreachable!("no commitment in the mempool")
    }
    fn verify(
        &self,
        _: &LlmqCommitment,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<bool, QcBlockError> {
        unreachable!("no commitment in the mempool")
    }
}

/// Platform quorums, each `(mined height, hash, key)`; a scan at a block returns the newest
/// mined by its height. The hashes are main-chain block hashes, so `LookupBlockIndex` finds them.
struct MinedQuorums<'a> {
    chain: &'a MemChain,
    quorums: &'a [(u32, QuorumHash, u8)],
}

impl QuorumLookup for MinedQuorums<'_> {
    fn get_quorum(
        &mut self,
        _: LlmqType,
        quorum_hash: &QuorumHash,
        _: &mut dyn EffectSink,
    ) -> Result<Option<QuorumKey>, SeamError> {
        Ok(self.quorums.iter().find(|q| q.1 == *quorum_hash).map(|&(_, quorum_hash, key)| {
            QuorumKey {
                quorum_hash,
                public_key: operator_key(key, BlsSchemeKind::Basic),
            }
        }))
    }

    fn scan_quorums(
        &mut self,
        _: LlmqType,
        at: BlockId,
        n: usize,
        out: &mut QuorumScan,
        _: &mut dyn EffectSink,
    ) -> Result<(), SeamError> {
        let height = self.chain.header(at).height;
        out.clear();
        let mut mined: Vec<_> = self.quorums.iter().filter(|q| q.0 <= height).collect();
        mined.sort_by_key(|q| std::cmp::Reverse(q.0));
        for q in mined.into_iter().take(n) {
            out.push(q.1)?;
        }
        Ok(())
    }
}

struct Index<'a>(&'a MemChain, BlockId);

impl BlockLookup for Index<'_> {
    fn lookup(&self, hash: &BlockHash) -> Option<(BlockId, u32)> {
        let tip = self.0.header(self.1).height;
        (0..=tip)
            .filter_map(|h| self.0.ancestor(self.1, h))
            .find(|&id| self.0.hash(id) == *hash)
            .map(|id| (id, self.0.header(id).height))
    }
}

/// The active chain, with `CheckSpecialTx` as dash-dml runs it for the mempool over the platform
/// quorums of [`MinedQuorums`]; each check's `pindexPrev` height is recorded.
struct DmlChain {
    chain: MemChain,
    tip: BlockId,
    quorums: Vec<(u32, QuorumHash, u8)>,
    checked_at: Vec<u32>,
}

/// Base heights 1000, 1004, …, 1240: quorum `i` is based at `1000 + 4i` and mined two blocks
/// later, under key `i + 1`.
const QUORUMS: u32 = 61;

impl DmlChain {
    fn new(tip_height: u32) -> DmlChain {
        let mut chain = MemChain::new();
        let main = chain.extend(chain.genesis(), 1_300);
        let quorums = (0..QUORUMS)
            .map(|i| {
                let base = chain.ancestor(main, 1_000 + 4 * i).expect("main chain");
                (1_002 + 4 * i, QuorumHash::from(*chain.hash(base).as_bytes()), i as u8 + 1)
            })
            .collect();
        let tip = chain.ancestor(main, tip_height).expect("main chain");
        DmlChain {
            chain,
            tip,
            quorums,
            checked_at: Vec::new(),
        }
    }

    /// The quorum mined at `mined`.
    fn mined_at(&self, mined: u32) -> (QuorumHash, u8) {
        let q = self.quorums.iter().find(|q| q.0 == mined).expect("a quorum mined there");
        (q.1, q.2)
    }

    /// A version-2 unlock of index 7 requested at `requested`, naming the quorum mined at `mined`
    /// and signed by `key` over Core's `SignHash` (`evo/assetlocktx.cpp:144-147`).
    fn unlock(&self, requested: u32, mined: u32, key: Option<u8>) -> TxRef {
        let (quorum, quorum_key) = self.mined_at(mined);
        let p = unlock_payload(7, requested, quorum, [0; 96]);
        let msg = sha256d(&p.sig_hash_bytes(unlock_tx(&p).raw()));
        let llmq_type = params().consensus.llmq_type_platform;
        let hash = sign_hash(llmq_type, &quorum, p.request_id().as_bytes(), &msg);
        let sig = operator_sig(key.unwrap_or(quorum_key), &hash).to_bytes(BlsSchemeKind::Basic);
        unlock_tx(&unlock_payload(7, requested, quorum, sig))
    }
}

impl TipFacts for DmlChain {
    fn tip(&self) -> Option<BlockId> {
        Some(self.tip)
    }
    fn ancestry(&self) -> &dyn ChainAncestry {
        &self.chain
    }
}

impl SpecialTxChain for DmlChain {
    fn check_special_tx(
        &mut self,
        tx: &Tx,
        pindex: BlockId,
        rules: SpecialTxRules,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        assert_eq!(rules, v24());
        let params = params();
        let header = self.chain.header(pindex);
        self.checked_at.push(header.height);
        let cons = ConsensusCtx {
            params,
            height: header.height + 1,
            prev: Some(pindex),
            chain: &self.chain,
            deployments: &AllActive,
            checkpoints: &AllActive,
            bls: BlsCtx::BASIC,
            median_time_past: header.mtp,
            header_check_time: 0,
        };
        let complete = CompleteChain::<StoreError>::new(&self.chain);
        let llmq = LlmqCtx::new(params, &complete, &AllActive, BlsCtx::BASIC).expect("in domain");
        let index = Index(&self.chain, self.tip);
        let mut quorums = MinedQuorums {
            chain: &self.chain,
            quorums: &self.quorums,
        };
        let mut effects = Vec::new();
        let list = MnList::default_pre_dip3();
        let coins = InputView::default();
        let mut cx = SpecialTxCtx::<Infallible> {
            evo: EvoCtx {
                cons: &cons,
                check_sigs: true,
            },
            rules: specialtx::SpecialTxRules {
                v24: true,
                ..Default::default()
            },
            list: &list,
            coins: &coins,
            net_info: NetInfoRules::new(params, mainnet(), CORE_NET_INFO),
            verifier: &SerialVerifier,
            quorum: Some(QuorumSeams {
                llmq,
                index: &index,
                commitments: &NoCommitments,
                quorums: &mut quorums,
                effects: &mut effects,
            }),
            unlock_indexes: None,
        };
        check_special_tx_inner(tx, tx.payload(), &mut cx)
            .map_err(|fault| Fatal::new(format!("{fault:?}")))
    }
}

fn v24() -> SpecialTxRules {
    SpecialTxRules {
        v24: true,
        ..Default::default()
    }
}

/// Core's default `-mempoolexpiry` (336 h) and mainnet's spacing (150 s).
const EXPIRY: i64 = 336 * 3600;

fn mempool_check(
    chain: &mut DmlChain,
    tx: &Tx,
    expiry: i64,
) -> Result<(), (TxValidationResult, String)> {
    let spacing = params().consensus.pow_target_spacing;
    check_special_tx_for_mempool(chain, tx, v24(), expiry, spacing)
        .expect("no fault")
        .map_err(|e| (e.result(), e.reject_reason().to_owned()))
}

fn bad_special(reason: &str) -> Result<(), (TxValidationResult, String)> {
    Err((TxValidationResult::BadSpecial, reason.to_owned()))
}

/// MPIX-16: an asset unlock checked at the tip outside the receiver's window is
/// `bad-assetunlock-too-old-quorum` or `bad-assetunlock-too-late`, `TX_BAD_SPECIAL`
/// (`evo/assetlocktx.cpp:125-135`), whenever `CheckSpecialTxForMempool` checks it at the tip
/// (`v:822-825`: its window still open, or begun before `m_expiry`).
#[test]
fn mpix16_unlock_outside_window_is_bad_special_at_tip() {
    // The window [1180, 1227] is still open at 1200; the quorum mined at 1002 has 49 newer ones.
    let mut chain = DmlChain::new(1_200);
    let tx = chain.unlock(1_180, 1_002, None);
    assert_eq!(mempool_check(&mut chain, &tx, EXPIRY), bad_special(BAD_ASSETUNLOCK_TOO_OLD_QUORUM));
    assert_eq!(chain.checked_at, vec![1_200]);

    // The window [1020, 1067] closed below 1100 and began before the expiry cutoff (1100 - 79).
    let mut chain = DmlChain::new(1_100);
    let tx = chain.unlock(1_020, 1_030, None);
    assert_eq!(mempool_check(&mut chain, &tx, 79 * 150), bad_special(BAD_ASSETUNLOCK_TOO_LATE));
    assert_eq!(chain.checked_at, vec![1_100]);
    let tip_verdict = check_special_tx_at_tip(&mut chain, &tx, v24())
        .expect("no fault")
        .map_err(|e| (e.result(), e.reject_reason().to_owned()));
    assert_eq!(tip_verdict, bad_special(BAD_ASSETUNLOCK_TOO_LATE), "TryAssetUnlockRefresh's check");
}

/// MPIX-16, the expired-window retry (`v:827-836`): an expired unlock inside `m_expiry` is checked
/// at `chain[first]`, and only after `bad-assetunlock-too-old-quorum` there, from a fresh state at
/// `chain[last]`. A quorum mined after the window's first height but active at its last admits
/// it; the cutoff `first < tip - m_expiry / spacing` is strict.
#[test]
fn mpix16_expired_window_retry_admits() {
    // Quorum mined at 1030: unknown at 1020, the 10th newest at 1067.
    let mut chain = DmlChain::new(1_100);
    let tx = chain.unlock(1_020, 1_030, None);
    assert_eq!(mempool_check(&mut chain, &tx, EXPIRY), Ok(()));
    assert_eq!(chain.checked_at, vec![1_020, 1_067]);

    // The cutoff at 1100 - 80 = 1020 = first: still the window path.
    chain.checked_at.clear();
    assert_eq!(mempool_check(&mut chain, &tx, 80 * 150), Ok(()));
    assert_eq!(chain.checked_at, vec![1_020, 1_067]);

    // Quorum mined at 1010, active at 1020: admitted at the first check.
    let mut chain = DmlChain::new(1_100);
    let tx = chain.unlock(1_020, 1_010, None);
    assert_eq!(mempool_check(&mut chain, &tx, EXPIRY), Ok(()));
    assert_eq!(chain.checked_at, vec![1_020]);

    // Another failure at the first check is returned without the retry.
    let mut chain = DmlChain::new(1_100);
    let tx = chain.unlock(1_020, 1_010, Some(60));
    assert_eq!(
        mempool_check(&mut chain, &tx, EXPIRY),
        Err((TxValidationResult::Consensus, BAD_ASSETUNLOCK_NOT_VERIFIED.to_owned()))
    );
    assert_eq!(chain.checked_at, vec![1_020]);

    // Too old at both ends: the last check's verdict.
    let mut chain = DmlChain::new(1_100);
    let tx = chain.unlock(1_020, 1_070, None);
    assert_eq!(mempool_check(&mut chain, &tx, EXPIRY), bad_special(BAD_ASSETUNLOCK_TOO_OLD_QUORUM));
    assert_eq!(chain.checked_at, vec![1_020, 1_067]);
}

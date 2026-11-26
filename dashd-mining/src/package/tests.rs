use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::coins::{Coin, CoinRef, CoinWrite, MemoryCoins};
use dash_consensus::primitives::TxRef;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_dml::specialtx::covenant::SHARED_COLLATERAL_SCRIPT;
use dash_dml::{Dmn, StoreError};
use dash_evo::EffectSink;
use dash_evo::llmq::{MemberError, QuorumMemberSource, QuorumMembers};
use dash_params::LlmqType;
use dash_primitives::{BlockHash, OutPoint, Transaction, TxIn, TxOut};

use super::*;
use crate::select::tests::Dag;
use crate::testkit::FakePool;

mod rules;

const HEIGHT: u32 = 1000;
const CUTOFF: i64 = 1_700_000_000;
pub(crate) const CELL_FAULT: &str = "coins cell unavailable";

/// Whether a coins guard is out: the hooks must never run while one is.
type Held = Arc<AtomicBool>;

/// V's tip: `CheckSpecialTx` verdicts by txid, the coins, and every call made.
#[derive(Default)]
pub(crate) struct FakeTip {
    pub coins: MemoryCoins,
    /// `CheckSpecialTx(check_sigs=false)` rejects.
    pub reject: HashMap<TxHash, &'static str>,
    /// `CheckSpecialTx(check_sigs=true)` rejects.
    pub reject_signed: HashMap<TxHash, &'static str>,
    /// A read fault on this transaction's `CheckSpecialTx`.
    pub fault: Option<TxHash>,
    /// Every coins read fails.
    pub coins_fault: bool,
    /// The coins cell (and the LLMQ view) is unavailable: `with_tip_coins` and
    /// `with_list_inputs` fail without calling `f`.
    pub cell_fault: bool,
    pub special_calls: Mutex<Vec<(TxHash, bool)>>,
    pub coins_takes: AtomicUsize,
    pub list_calls: AtomicUsize,
    pub held: Held,
}

/// The fake's `CoinsTip()`, whose reads fail on request.
struct TipCoins<'a> {
    coins: &'a MemoryCoins,
    fault: bool,
}

impl CoinsView for TipCoins<'_> {
    type Error = &'static str;

    fn coin(&self, outpoint: &OutPoint) -> Result<Option<CoinRef<'_>>, &'static str> {
        if self.fault {
            return Err("injected coins fault");
        }
        Ok(self.coins.coin(outpoint).unwrap_or_else(|never| match never {}))
    }

    fn best_block(&self) -> Option<BlockHash> {
        self.coins.best_block()
    }
}

/// No quorum has members.
struct NoMembers;

impl QuorumMemberSource for NoMembers {
    type Entry = Arc<Dmn>;
    type Error = StoreError;

    fn members(
        &self,
        _: LlmqType,
        base: dash_consensus::BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        Err(MemberError::Missing(base))
    }
}

impl FakeTip {
    pub fn special_calls(&self) -> Vec<(TxHash, bool)> {
        self.special_calls.lock().unwrap().clone()
    }

    pub fn coins_takes(&self) -> usize {
        self.coins_takes.load(Ordering::SeqCst)
    }

    pub fn list_calls(&self) -> usize {
        self.list_calls.load(Ordering::SeqCst)
    }

    fn cell(&self) -> Result<(), BlockError> {
        if self.cell_fault {
            return Err(BlockError::Fault(CELL_FAULT.to_owned()));
        }
        Ok(())
    }

    fn take<R>(&self, f: impl FnOnce(&TipCoins<'_>) -> R) -> R {
        assert!(!self.held.swap(true, Ordering::SeqCst), "coins taken twice");
        let out = f(&TipCoins {
            coins: &self.coins,
            fault: self.coins_fault,
        });
        self.held.store(false, Ordering::SeqCst);
        out
    }
}

pub(crate) fn reject(reason: &'static str) -> TxValidationError {
    TxValidationError::new(TxValidationResult::Consensus, reason)
}

impl TipReads for FakeTip {
    type CoinsError = &'static str;

    fn check_special_tx(
        &self,
        tx: &Tx,
        check_sigs: bool,
    ) -> Result<Result<(), TxValidationError>, BlockError> {
        let txid = tx.txid();
        self.special_calls.lock().unwrap().push((txid, check_sigs));
        if self.fault == Some(txid) {
            return Err(BlockError::Fault(format!("read fault at {txid}")));
        }
        let rejects = if check_sigs {
            &self.reject_signed
        } else {
            &self.reject
        };
        Ok(rejects.get(&txid).map_or(Ok(()), |reason| Err(reject(reason))))
    }

    fn with_tip_coins<R>(
        &self,
        f: impl FnOnce(&dyn CoinsView<Error = Self::CoinsError>) -> R,
    ) -> Result<R, BlockError> {
        self.coins_takes.fetch_add(1, Ordering::SeqCst);
        self.cell()?;
        Ok(self.take(|coins| f(coins)))
    }

    fn with_list_inputs<R>(
        &self,
        f: impl FnOnce(&dyn CoinsView<Error = Self::CoinsError>, &DmlMemberSource<'_>) -> R,
    ) -> Result<R, BlockError> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        self.cell()?;
        Ok(self.take(|coins| f(coins, &NoMembers)))
    }
}

/// IS and ChainLock state; records the safety questions, and fails if a coins guard is out.
#[derive(Default)]
pub(crate) struct Hooks {
    pub enabled: bool,
    pub locked: HashSet<TxHash>,
    pub safe: HashSet<TxHash>,
    pub asked: Mutex<Vec<TxHash>>,
    pub held: Held,
}

impl Hooks {
    fn not_held(&self) {
        assert!(!self.held.load(Ordering::SeqCst), "a hook ran with the coins taken");
    }

    pub fn asked(&self) -> Vec<TxHash> {
        self.asked.lock().unwrap().clone()
    }
}

impl InstantSendHooks for Hooks {
    fn is_enabled(&self) -> bool {
        self.not_held();
        self.enabled
    }

    fn is_locked(&self, txid: &TxHash) -> bool {
        self.not_held();
        self.locked.contains(txid)
    }
}

impl MiningSafety for Hooks {
    fn is_tx_safe_for_mining(&self, txid: &TxHash) -> bool {
        self.not_held();
        self.asked.lock().unwrap().push(*txid);
        self.safe.contains(txid)
    }
}

/// A tip and hooks sharing one held flag.
pub(crate) fn tip_and_hooks() -> (FakeTip, Hooks) {
    let held = Held::default();
    let tip = FakeTip {
        held: Arc::clone(&held),
        ..FakeTip::default()
    };
    let hooks = Hooks {
        held,
        ..Hooks::default()
    };
    (tip, hooks)
}

pub(crate) fn list() -> &'static MnList {
    static LIST: std::sync::OnceLock<MnList> = std::sync::OnceLock::new();
    LIST.get_or_init(MnList::scratch)
}

pub(crate) fn inputs<'r>(
    tip: &'r FakeTip,
    hooks: &'r Hooks,
    v24: bool,
) -> PackageInputs<'r, 'r, FakeTip> {
    PackageInputs {
        tip,
        instant_send: hooks,
        safety: hooks,
        dmn_tip_list: list(),
        height: HEIGHT,
        lock_time_cutoff: CUTOFF,
        v24,
        credit_pool: None,
        signals: EhfSignals::new(),
        builder: None,
    }
}

pub(crate) fn rules<'r>(
    tip: &'r FakeTip,
    hooks: &'r Hooks,
    v24: bool,
) -> PackageRules<'r, 'r, FakeTip> {
    PackageRules::new(inputs(tip, hooks, v24))
}

pub(crate) fn input(n: u8, sequence: u32) -> TxIn {
    TxIn::new(OutPoint::new(TxHash::from([n; 32]), 0), Default::default(), sequence)
}

/// A transaction; `tag` keeps txids apart.
pub(crate) fn raw_tx(
    version: i16,
    tx_type: TxType,
    inputs: Vec<TxIn>,
    outputs: Vec<TxOut>,
    lock_time: u32,
    payload: Vec<u8>,
) -> TxRef {
    Arc::new(Tx::new(Transaction::new(version, tx_type, inputs, outputs, lock_time, payload)))
}

pub(crate) fn out(script: &[u8]) -> TxOut {
    TxOut::from_duffs(1, script.to_vec().into())
}

/// An ordinary transaction spending `[tag; 32]:0`.
pub(crate) fn ordinary(tag: u8) -> TxRef {
    raw_tx(1, TxType::Spend, vec![input(tag, u32::MAX)], vec![out(&[0x51])], 0, Vec::new())
}

/// A special transaction of `tx_type` with an opaque payload; `CheckSpecialTx` is the fake's.
pub(crate) fn special(tag: u8, tx_type: TxType) -> TxRef {
    raw_tx(3, tx_type, vec![input(tag, u32::MAX)], vec![out(&[0x51])], 0, vec![tag])
}

/// A ProUpServTx (payload version 1) of `pro`.
pub(crate) fn pro_up_serv(tag: u8, pro: ProTxHash) -> TxRef {
    let mut p = Vec::new();
    p.extend(1u16.to_le_bytes());
    p.extend(pro.as_bytes());
    p.extend([0; 10]);
    p.extend([0xff, 0xff, 127, 0, 0, 1]);
    p.extend(9999u16.to_be_bytes());
    p.push(0);
    p.extend([tag; 32]);
    p.extend([0; 96]);
    let tx = raw_tx(
        3,
        TxType::ProviderUpdateService,
        vec![input(tag, u32::MAX)],
        vec![out(&[0x51])],
        0,
        p,
    );
    assert_eq!(service_update_target(&tx), Some(pro), "a complete ProUpServTx payload");
    tx
}

pub(crate) fn mn(n: u8) -> ProTxHash {
    let mut bytes = [0; 32];
    bytes[0] = n;
    ProTxHash::from(bytes)
}

pub(crate) fn txid(pool: &FakePool, id: EntryId) -> TxHash {
    pool.mining_entry(id).txid
}

pub(crate) fn single(tx: TxRef) -> (FakePool, EntryId) {
    let mut dag = Dag::default();
    let id = dag.add_tx(tx, &[], 1000, 100, 1);
    (dag.pool(), id)
}

/// Core v24.0.2 src/node/miner.cpp:396: `IsFinalTx(tx, nHeight, m_lock_time_cutoff)`, by height
/// below the threshold and by the cutoff above it; a final sequence ignores the lock time.
#[test]
fn is_final_tx_at_the_template_height_and_cutoff() {
    let (tip, hooks) = tip_and_hooks();
    let rules = rules(&tip, &hooks, false);
    let locked = |lock_time, sequence| {
        raw_tx(1, TxType::Spend, vec![input(1, sequence)], vec![out(&[0x51])], lock_time, vec![])
    };
    let cutoff = CUTOFF as u32;
    for (lock_time, sequence, final_) in [
        (HEIGHT - 1, 0, true),
        (HEIGHT, 0, false),
        (HEIGHT, u32::MAX, true),
        (cutoff - 1, 0, true),
        (cutoff, 0, false),
    ] {
        let (pool, id) = single(locked(lock_time, sequence));
        assert_eq!(rules.test_package_transactions(&pool, &[id]), final_, "lock time {lock_time}");
    }
}

/// Core v24.0.2 src/node/miner.cpp:409-414: special transactions only, `check_sigs` off, and a
/// reject fails the package.
#[test]
fn check_special_tx_without_signatures_on_special_transactions_only() {
    let (mut tip, hooks) = tip_and_hooks();
    let plain = ordinary(1);
    let good = special(2, TxType::ProviderUpdateService);
    let bad = special(3, TxType::AssetLock);
    tip.reject.insert(bad.txid(), "bad-protx-dup-addr");
    let mut dag = Dag::default();
    let plain = dag.add_tx(plain, &[], 1000, 100, 1);
    let good = dag.add_tx(good, &[], 1000, 100, 1);
    let bad = dag.add_tx(bad, &[], 1000, 100, 1);
    let pool = dag.pool();
    let rules = rules(&tip, &hooks, false);
    assert!(rules.test_package_transactions(&pool, &[plain, good]));
    assert_eq!(tip.special_calls(), vec![(txid(&pool, good), false)]);
    assert!(!rules.test_package_transactions(&pool, &[bad]));
    assert_eq!(tip.special_calls()[1], (txid(&pool, bad), false));
}

/// Core v24.0.2 src/node/miner.cpp:396-436: the first failing check ends the package; later
/// checks and later transactions are not run.
#[test]
fn the_first_failure_wins() {
    let (mut tip, mut hooks) = tip_and_hooks();
    hooks.enabled = true;
    let nonfinal =
        raw_tx(3, TxType::AssetLock, vec![input(1, 0)], vec![out(&[0x51])], HEIGHT, vec![1]);
    let rejected = special(2, TxType::AssetLock);
    tip.reject.insert(rejected.txid(), "bad-assetlocktx-type");
    let (pool, id) = single(nonfinal);
    let rules = rules(&tip, &hooks, true);
    assert!(!rules.test_package_transactions(&pool, &[id]));
    assert_eq!((tip.special_calls(), tip.coins_takes(), hooks.asked()), (vec![], 0, vec![]));

    let (pool, id) = single(rejected);
    assert!(!rules.test_package_transactions(&pool, &[id]));
    assert_eq!(tip.special_calls().len(), 1);
    assert_eq!(tip.coins_takes(), 0, "the covenant runs after CheckSpecialTx");
    assert!(hooks.asked().is_empty());

    let mut dag = Dag::default();
    let unsafe_ = dag.add_tx(ordinary(3), &[], 1000, 100, 1);
    let after = dag.add_tx(special(4, TxType::AssetLock), &[], 1000, 100, 1);
    let pool = dag.pool();
    assert!(!rules.test_package_transactions(&pool, &[unsafe_, after]));
    assert_eq!(tip.special_calls().len(), 1, "nothing after the unsafe transaction");
    assert_eq!(hooks.asked(), vec![txid(&pool, unsafe_)]);
}

impl FakeTip {
    fn with_template_coin(mut self, outpoint: OutPoint) -> Self {
        let coin = Coin::new(1000, &SHARED_COLLATERAL_SCRIPT, 1, false);
        self.coins.write(outpoint, CoinWrite::Put(coin));
        self
    }
}

/// Core v24.0.2 src/node/miner.cpp:421-428: with V24, every transaction (an ordinary one too)
/// meets the covenant over the pre-block coins: no spend of a template output but a ProDisTx's,
/// no template output but a shared ProRegTx's. Without V24 neither is checked.
#[test]
fn v24_covenant_on_an_ordinary_transaction() {
    let template_outpoint = OutPoint::new(TxHash::from([9; 32]), 0);
    let (tip, hooks) = tip_and_hooks();
    let tip = tip.with_template_coin(template_outpoint);
    let spender = raw_tx(
        1,
        TxType::Spend,
        vec![TxIn::new(template_outpoint, Default::default(), u32::MAX)],
        vec![out(&[0x51])],
        0,
        vec![],
    );
    let creator = raw_tx(
        1,
        TxType::Spend,
        vec![input(1, u32::MAX)],
        vec![out(&SHARED_COLLATERAL_SCRIPT)],
        0,
        vec![],
    );
    let plain = ordinary(2);
    let mut dag = Dag::default();
    let spender = dag.add_tx(spender, &[], 1000, 100, 1);
    let creator = dag.add_tx(creator, &[], 1000, 100, 1);
    let plain = dag.add_tx(plain, &[], 1000, 100, 1);
    let pool = dag.pool();

    let v24 = rules(&tip, &hooks, true);
    assert!(!v24.test_package_transactions(&pool, &[spender]));
    assert!(!v24.test_package_transactions(&pool, &[creator]));
    assert!(v24.test_package_transactions(&pool, &[plain]));
    assert_eq!(tip.coins_takes(), 3, "one coins guard per check");

    let pre = rules(&tip, &hooks, false);
    assert!(pre.test_package_transactions(&pool, &[spender, creator, plain]));
}

/// Core v24.0.2 src/node/miner.cpp:421-428: the spend check reads the pre-block coins, never the
/// outputs of transactions already selected: a template output created in the mempool is no coin.
#[test]
fn the_covenant_reads_pre_block_coins_only() {
    let (tip, hooks) = tip_and_hooks();
    let parent = raw_tx(
        3,
        TxType::ProviderRegister,
        vec![input(1, u32::MAX)],
        vec![out(&SHARED_COLLATERAL_SCRIPT)],
        0,
        vec![1],
    );
    let child = raw_tx(
        1,
        TxType::Spend,
        vec![TxIn::new(OutPoint::new(parent.txid(), 0), Default::default(), u32::MAX)],
        vec![out(&[0x51])],
        0,
        vec![],
    );
    let (pool, id) = single(child);
    assert!(rules(&tip, &hooks, true).test_package_transactions(&pool, &[id]));
}

/// Core v24.0.2 src/node/miner.cpp:430-436: IS disabled or the transaction locked → no safety
/// question; else a transaction with inputs must be safe, one without needs nothing.
#[test]
fn instant_send_enabled_locked_unsafe_matrix() {
    let with_inputs = ordinary(1);
    let without_inputs = raw_tx(3, TxType::AssetUnlock, vec![], vec![out(&[0x51])], 0, vec![2]);
    for (enabled, locked, safe, inputs, pass, asked) in [
        (false, false, false, true, true, false),
        (false, true, false, true, true, false),
        (true, true, false, true, true, false),
        (true, false, false, true, false, true),
        (true, false, true, true, true, true),
        (true, false, false, false, true, false),
    ] {
        let (tip, mut hooks) = tip_and_hooks();
        let tx = if inputs {
            with_inputs.clone()
        } else {
            without_inputs.clone()
        };
        hooks.enabled = enabled;
        if locked {
            hooks.locked.insert(tx.txid());
        }
        if safe {
            hooks.safe.insert(tx.txid());
        }
        let (pool, id) = single(tx.clone());
        let case = (enabled, locked, safe, inputs);
        assert_eq!(
            rules(&tip, &hooks, true).test_package_transactions(&pool, &[id]),
            pass,
            "{case:?}"
        );
        assert_eq!(hooks.asked() == vec![tx.txid()], asked, "{case:?}");
    }
}

/// A read fault ends the rules: the package fails, later checks run nothing, and `finish`
/// returns the fault.
#[test]
fn a_read_fault_is_latched() {
    let (mut tip, hooks) = tip_and_hooks();
    let faulty = special(1, TxType::AssetLock);
    tip.fault = Some(faulty.txid());
    let mut dag = Dag::default();
    let faulty = dag.add_tx(faulty, &[], 1000, 100, 1);
    let next = dag.add_tx(special(2, TxType::AssetLock), &[], 1000, 100, 1);
    let pool = dag.pool();
    let rules = rules(&tip, &hooks, false);
    assert!(!rules.test_package_transactions(&pool, &[faulty]));
    assert!(!rules.test_package_transactions(&pool, &[next]));
    assert_eq!(tip.special_calls().len(), 1);
    let fault = format!("read fault at {}", txid(&pool, faulty));
    assert_eq!(rules.finish().err(), Some(BlockError::Fault(fault)));
}

/// The coins cell unavailable to the covenant is a read fault, not a pass.
#[test]
fn an_unavailable_coins_cell_in_the_covenant_is_a_read_fault() {
    let (mut tip, hooks) = tip_and_hooks();
    tip.cell_fault = true;
    let (pool, id) = single(ordinary(1));
    let rules = rules(&tip, &hooks, true);
    assert!(!rules.test_package_transactions(&pool, &[id]));
    assert_eq!(tip.coins_takes(), 1);
    assert_eq!(rules.finish().err(), Some(BlockError::Fault(CELL_FAULT.to_owned())));
}

/// Core v24.0.2 src/node/miner.cpp:651-671: each update with its unconfirmed ancestors, unless
/// already in the block or already taken; the order is a `setEntries`.
#[test]
fn service_updates_come_with_their_unconfirmed_ancestors() {
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let key_change = dag.add(&[], 1000, 100, 1);
    let mined = dag.add(&[], 1000, 100, 1);
    let parent = dag.add(&[mined], 1000, 100, 1);
    let update = dag.add_tx(pro_up_serv(1, mn(1)), &[parent], 1000, 100, 1);
    let second = dag.add_tx(pro_up_serv(2, mn(1)), &[update], 1000, 100, 1);
    let mut pool = dag.pool();
    pool.set_service_updates(key_change, vec![second, update]);
    let in_block = HashSet::from([mined]);
    let rules = rules(&tip, &hooks, false);
    let updates = rules.service_updates(&pool, &[key_change], &in_block);
    let mut expected = vec![parent, update, second];
    set_entries(&pool, &mut expected);
    let mut got = updates.clone();
    set_entries(&pool, &mut got);
    assert_eq!((got, updates.len()), (expected, 3));

    // A unit probe of the gather: the selector never puts `update` in the block without `parent`.
    let in_block = HashSet::from([mined, update]);
    let updates = rules.service_updates(&pool, &[key_change], &in_block);
    assert_eq!(HashSet::<EntryId>::from_iter(updates), HashSet::from([parent, second]));
}

/// Core v24.0.2 src/node/miner.cpp:662-670: an update package holding a ProUpServTx of a
/// masternode in `keyChangedMNs` is left out, before `TestPackageTransactions` runs.
#[test]
fn an_update_following_a_key_change_is_left_out() {
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let key_change = dag.add(&[], 1000, 100, 1);
    let ancestor = dag.add_tx(pro_up_serv(1, mn(7)), &[], 1000, 100, 1);
    let update = dag.add_tx(pro_up_serv(2, mn(1)), &[ancestor], 1000, 100, 1);
    let other = dag.add_tx(pro_up_serv(3, mn(1)), &[], 1000, 100, 1);
    let mut pool = dag.pool();
    pool.set_service_updates(key_change, vec![update, other]);
    let mut rules = rules(&tip, &hooks, false);
    rules.key_changed.insert(mn(7));
    let updates = rules.service_updates(&pool, &[key_change], &HashSet::new());
    assert_eq!(updates, vec![other]);
    assert_eq!(tip.special_calls(), vec![(txid(&pool, other), false)]);
}

/// Core v24.0.2 src/node/miner.cpp:667-668: an update package `TestPackageTransactions` refuses,
/// or an unorderable update, is left out; the others stay.
#[test]
fn a_refused_or_unorderable_update_is_left_out() {
    let (mut tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let key_change = dag.add(&[], 1000, 100, 1);
    let refused = pro_up_serv(1, mn(1));
    tip.reject.insert(refused.txid(), "bad-protx-sig");
    let refused = dag.add_tx(refused, &[], 1000, 100, 1);
    let unorderable = dag.add_tx(pro_up_serv(2, mn(1)), &[], 1000, 100, 1);
    let kept = dag.add_tx(pro_up_serv(3, mn(1)), &[], 1000, 100, 1);
    let mut pool = dag.pool();
    pool.set_service_updates(key_change, vec![refused, unorderable, kept]);
    pool.set_unorderable(unorderable, true);
    let rules = rules(&tip, &hooks, false);
    assert_eq!(rules.service_updates(&pool, &[key_change], &HashSet::new()), vec![kept]);
    pool.set_unorderable(unorderable, false);
    let mut got = rules.service_updates(&pool, &[key_change], &HashSet::new());
    got.sort();
    assert_eq!(got, vec![unorderable, kept]);
}

/// Core v24.0.2 src/node/miner.cpp:652-653: every package member's updates are gathered, each
/// once.
#[test]
fn updates_of_every_package_member_each_once() {
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let first = dag.add(&[], 1000, 100, 1);
    let second = dag.add(&[first], 1000, 100, 1);
    let shared = dag.add_tx(pro_up_serv(1, mn(1)), &[], 1000, 100, 1);
    let own = dag.add_tx(pro_up_serv(2, mn(2)), &[], 1000, 100, 1);
    let mut pool = dag.pool();
    pool.set_service_updates(first, vec![shared]);
    pool.set_service_updates(second, vec![shared, own]);
    let rules = rules(&tip, &hooks, false);
    let mut got = rules.service_updates(&pool, &[first, second], &HashSet::new());
    got.sort();
    assert_eq!(got, vec![shared, own]);
    assert_eq!(tip.special_calls().len(), 2, "the shared update is tested once");
}

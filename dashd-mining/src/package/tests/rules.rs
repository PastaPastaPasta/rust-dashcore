//! The per-package rules through MN1-2's selector: Core's log line for each skip, what a package
//! commits, the list trial on a real `CMNListBuilder` and the credit pool.

use std::cell::RefCell;
use std::sync::Once;

use dash_consensus::ChainAncestry as _;
use dash_dml::creditpool::CreditPool;
use dash_dml::netinfo::{NetInfoConfig, NetInfoRules};
use dash_dml::testkit::{MemChain, dmn, legacy_operator_key};
use dash_dml::{DmlParams, DmlRules, MnType, PrevBlock};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};

use super::*;
use crate::options::{FeeRate, Options};
use crate::select::{BlockState, add_package_txs};

thread_local! {
    static LOG: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Routes this crate's events to the calling thread's log while one is open.
struct ThreadLog;

impl tracing::Subscriber for ThreadLog {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.is_event()
            && meta.target().starts_with("dashd_mining")
            && LOG.with(|log| log.borrow().is_some())
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        LOG.with(|log| {
            if let Some(lines) = log.borrow_mut().as_mut() {
                lines.push(message.0);
            }
        });
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// The log lines `f` emits on this thread.
fn logged<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(ThreadLog).expect("no other global subscriber");
    });
    tracing::callsite::rebuild_interest_cache();
    LOG.with(|log| *log.borrow_mut() = Some(Vec::new()));
    let out = f();
    let lines = LOG.with(|log| log.borrow_mut().take()).unwrap_or_default();
    (out, lines)
}

/// `addPackageTxs` with the rules; the block order and the log.
fn select(
    pool: &FakePool,
    rules: &mut PackageRules<'_, '_, FakeTip>,
) -> (Vec<EntryId>, Vec<String>) {
    select_sized(pool, rules, 1_999_000)
}

fn select_sized(
    pool: &FakePool,
    rules: &mut PackageRules<'_, '_, FakeTip>,
    max_size: u64,
) -> (Vec<EntryId>, Vec<String>) {
    let options = Options {
        block_max_size: max_size,
        block_max_sigops: 40_000,
        block_min_fee_rate: FeeRate::new(1000),
        ..Options::default()
    };
    let (selection, log) = logged(|| add_package_txs(pool, rules, &options, BlockState::default()));
    (selection.entries.iter().map(|s| s.id).collect(), log)
}

fn follows(pool: &FakePool, id: EntryId, mn: ProTxHash) -> String {
    format!(
        "addPackageTxs: package tx {} skipped, it would follow a key change of masternode {mn}",
        txid(pool, id)
    )
}

/// An `MNHFTxPayload` (version 1) signalling `bit`, spending `[tag; 32]:0`.
fn mnhf(tag: u8, bit: u8, sequence: u32, lock_time: u32) -> TxRef {
    let mut p = vec![1, bit];
    p.extend([0; 32 + 96]);
    let tx =
        raw_tx(3, TxType::MnhfSignal, vec![input(tag, sequence)], vec![out(&[0x51])], lock_time, p);
    assert_eq!(ehf_signal(&tx), Some(bit), "a complete MNHF payload");
    tx
}

/// An asset unlock of `index`; payload version 2 has a stable txid.
fn asset_unlock(version: u8, index: u64) -> TxRef {
    let mut p = vec![version];
    p.extend(index.to_le_bytes());
    p.extend(1000u32.to_le_bytes());
    p.extend(HEIGHT.to_le_bytes());
    p.extend([0xab; 32]);
    p.extend([0; 96]);
    let tx = raw_tx(
        3,
        TxType::AssetUnlock,
        vec![],
        vec![TxOut::from_duffs(5000, vec![0x51].into())],
        0,
        p,
    );
    assert_eq!(tx.is_asset_unlock_with_stable_txid(), version >= 2);
    tx
}

/// An asset lock of `credit`; `burn` adds the OP_RETURN output the credit pool reads.
fn asset_lock(tag: u8, credit: i64, burn: bool) -> TxRef {
    const P2PKH: [u8; 3] = [0x76, 0xa9, 0x14];
    let mut p = vec![1, 1];
    p.extend(credit.to_le_bytes());
    p.push(P2PKH.len() as u8);
    p.extend(P2PKH);
    let mut outputs = vec![out(&[0x51])];
    if burn {
        outputs.push(TxOut::from_duffs(credit, vec![0x6a, 0x00].into()));
    }
    raw_tx(3, TxType::AssetLock, vec![input(tag, u32::MAX)], outputs, 0, p)
}

/// A ProRegTx (payload version 1) with operator key `key` and owner key `[tag; 20]`: its own
/// output 0 as collateral, or `collateral`.
fn pro_reg(tag: u8, key: u8, collateral: Option<OutPoint>) -> TxRef {
    let mut p = Vec::new();
    p.extend(1u16.to_le_bytes());
    p.extend(0u16.to_le_bytes());
    p.extend(0u16.to_le_bytes());
    let collateral = collateral.unwrap_or(OutPoint::new(TxHash::from([0; 32]), 0));
    p.extend(collateral.txid().as_bytes());
    p.extend(collateral.index().to_le_bytes());
    p.extend([0; 18]);
    p.extend([tag; 20]);
    p.extend(legacy_operator_key(key).to_bytes());
    p.extend([tag; 20]);
    p.extend(0u16.to_le_bytes());
    let script = [
        0x76, 0xa9, 0x14, tag, tag, tag, tag, tag, tag, tag, tag, tag, tag, tag, tag, tag, tag,
        tag, tag, tag, tag, tag, tag, 0x88, 0xac,
    ];
    p.push(script.len() as u8);
    p.extend(script);
    p.extend([tag; 32]);
    p.push(0);
    let tx = raw_tx(
        3,
        TxType::ProviderRegister,
        vec![input(tag, u32::MAX)],
        vec![TxOut::from_duffs(1000 * 100_000_000, vec![0x51].into())],
        0,
        p,
    );
    assert!(matches!(tx.payload(), Some(Ok(d)) if d.is_complete()), "a complete ProRegTx payload");
    tx
}

/// A ProUpRegTx (payload version 1) moving `pro` to operator key `key`.
fn pro_up_reg(tag: u8, pro: ProTxHash, key: u8) -> TxRef {
    let mut p = Vec::new();
    p.extend(1u16.to_le_bytes());
    p.extend(pro.as_bytes());
    p.extend(0u16.to_le_bytes());
    p.extend(legacy_operator_key(key).to_bytes());
    p.extend([tag; 20]);
    p.push(1);
    p.push(0x51);
    p.extend([tag; 32]);
    p.push(0);
    let tx = raw_tx(
        3,
        TxType::ProviderUpdateRegistrar,
        vec![input(tag, u32::MAX)],
        vec![out(&[0x51])],
        0,
        p,
    );
    assert!(
        matches!(tx.payload(), Some(Ok(d)) if d.is_complete()),
        "a complete ProUpRegTx payload"
    );
    tx
}

fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest params")
}

/// What a real `CMNListBuilder` borrows: a chain, the DML parameters and the NetInfo rules.
struct ListWorld {
    chain: MemChain,
    tip: dash_consensus::BlockId,
    dml: DmlParams,
    net_info: NetInfoRules,
    list: MnList,
}

impl ListWorld {
    /// Masternodes 1 and 2 at the tip of a 30-block chain.
    fn new() -> Self {
        let mut chain = MemChain::new();
        let tip = chain.extend(chain.genesis(), 30);
        let params = regtest();
        let main = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("mainnet");
        let header = chain.header(tip);
        let mut list = MnList::new(header.hash, header.height as i32, 2);
        list.insert_for_tests(dmn(1, MnType::Regular));
        list.insert_for_tests(dmn(2, MnType::Regular));
        ListWorld {
            dml: DmlParams::new(&params),
            net_info: NetInfoRules::new(
                &params,
                &main,
                NetInfoConfig {
                    cjdns_reachable: true,
                },
            ),
            chain,
            tip,
            list,
        }
    }

    fn builder(&self) -> MnListBuilder<'_, '_> {
        self.builder_at(true)
    }

    fn builder_at(&self, v24: bool) -> MnListBuilder<'_, '_> {
        let header = self.chain.header(self.tip);
        let prev = PrevBlock {
            id: self.tip,
            hash: header.hash,
            height: header.height as i32,
            chain: &self.chain,
        };
        let rules = DmlRules {
            params: &self.dml,
            net_info: &self.net_info,
            v19: true,
            mn_rr: true,
            v24,
        };
        MnListBuilder::begin(prev, &self.list, rules).expect("the tip's builder")
    }
}

/// Core v24.0.2 src/node/miner.cpp:642-708, 800-805: a pending ProUpServTx goes ahead of the key
/// change that would invalidate it, in the same package.
#[test]
fn a_pending_service_update_is_ordered_before_the_key_change() {
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let key_change = dag.add(&[], 10_000, 100, 1);
    let update = dag.add_tx(pro_up_serv(1, mn(1)), &[], 200, 100, 1);
    let mut pool = dag.pool();
    pool.set_key_change_target(key_change, mn(1));
    pool.set_service_updates(key_change, vec![update]);
    let mut rules = rules(&tip, &hooks, true);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!((ids, log), (vec![update, key_change], vec![]));
    assert_eq!(rules.key_changed, HashSet::from([mn(1)]));
}

/// Core v24.0.2 src/node/miner.cpp:651-670, 714-724: an unorderable update (an asset lock
/// ancestor) is not pulled in; the key change goes in alone and the update, once its own
/// candidate, is skipped as following it. A later key change's update package holding it is
/// left out before `TestPackageTransactions`.
#[test]
fn unorderable_and_following_updates_are_left_out_then_skipped() {
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let key_change_1 = dag.add(&[], 40_000, 100, 1);
    let key_change_2 = dag.add(&[], 30_000, 100, 1);
    let lock = dag.add_tx(asset_lock(9, 1000, true), &[], 300, 100, 1);
    let update_1 = dag.add_tx(pro_up_serv(1, mn(1)), &[lock], 2000, 100, 1);
    let update_2 = dag.add_tx(pro_up_serv(2, mn(2)), &[update_1], 200, 100, 1);
    let mut pool = dag.pool();
    pool.set_key_change_target(key_change_1, mn(1));
    pool.set_key_change_target(key_change_2, mn(2));
    pool.set_service_updates(key_change_1, vec![update_1]);
    pool.set_service_updates(key_change_2, vec![update_2]);
    pool.set_unorderable(update_1, true);
    let mut rules = rules(&tip, &hooks, true);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!(ids, vec![key_change_1, key_change_2, lock]);
    // update_1's package, then (once the lock is in) update_1 and update_2 from the modified set;
    // each sorts update_1 first.
    assert_eq!(log, vec![follows(&pool, update_1, mn(1)); 3]);
}

/// Core v24.0.2 src/node/miner.cpp:673-685, 714-724: updates that do not fit with the key change
/// are all dropped, the key change goes in, and each update is then skipped.
#[test]
fn updates_that_do_not_fit_are_all_cleared() {
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let key_change = dag.add(&[], 100_000, 100, 1);
    let update_1 = dag.add_tx(pro_up_serv(1, mn(1)), &[], 3000, 300, 1);
    let update_2 = dag.add_tx(pro_up_serv(2, mn(1)), &[], 2000, 300, 1);
    let mut pool = dag.pool();
    pool.set_key_change_target(key_change, mn(1));
    pool.set_service_updates(key_change, vec![update_1, update_2]);
    let mut rules = rules(&tip, &hooks, true);
    // 1000 + 100 + 300 fits under 1500, + 300 more does not.
    let (ids, log) = select_sized(&pool, &mut rules, 1500);
    assert_eq!(ids, vec![key_change]);
    assert_eq!(log, vec![follows(&pool, update_1, mn(1)), follows(&pool, update_2, mn(1))]);
}

/// Core v24.0.2 src/node/miner.cpp:716-724: a ProUpServTx after a key change of its masternode
/// in the same package (`packageKeyChangedMNs`) is skipped, and the package commits nothing.
#[test]
fn a_service_update_after_a_key_change_in_its_package_is_skipped() {
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let key_change = dag.add(&[], 1000, 100, 1);
    let update = dag.add_tx(pro_up_serv(1, mn(3)), &[key_change], 3000, 100, 1);
    let mut pool = dag.pool();
    pool.set_key_change_target(key_change, mn(3));
    let mut rules = rules(&tip, &hooks, true);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!(ids, vec![key_change]);
    // The package first, then the update alone once the key change is in.
    assert_eq!(log, vec![follows(&pool, update, mn(3)); 2]);
}

/// Core v24.0.2 src/node/miner.cpp:728-734: a signal for a bit in the stage, already in the block
/// or twice in the package is skipped.
#[test]
fn duplicate_ehf_bits_are_skipped() {
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let staged = dag.add_tx(mnhf(1, 3, u32::MAX, 0), &[], 50_000, 100, 1);
    let first = dag.add_tx(mnhf(2, 4, u32::MAX, 0), &[], 40_000, 100, 1);
    let again = dag.add_tx(mnhf(3, 4, u32::MAX, 0), &[], 30_000, 100, 1);
    let parent = dag.add_tx(mnhf(4, 6, u32::MAX, 0), &[], 1000, 100, 1);
    let twin = dag.add_tx(mnhf(5, 6, u32::MAX, 0), &[parent], 20_000, 100, 1);
    let pool = dag.pool();
    let mut inputs = inputs(&tip, &hooks, true);
    inputs.signals = EhfSignals::from_iter([(3, 900)]);
    let mut rules = PackageRules::new(inputs);
    let (ids, log) = select(&pool, &mut rules);
    let dup = |id, bit| {
        format!(
            "addPackageTxs: package tx {} skipped due to duplicate EHF signal {bit}",
            txid(&pool, id)
        )
    };
    assert_eq!(ids, vec![first, parent]);
    assert_eq!(log, vec![dup(staged, 3), dup(again, 4), dup(twin, 6), dup(twin, 6)]);
}

/// Core v24.0.2 src/node/miner.cpp:736-752: an asset unlock with a stable txid is checked again
/// with signatures (its height window or quorum may have passed); a version-1 one is not. Both
/// are credit-pool transactions.
#[test]
fn an_asset_unlock_out_of_its_window_is_skipped() {
    let (mut tip, hooks) = tip_and_hooks();
    let expired = asset_unlock(2, 7);
    let fresh = asset_unlock(2, 8);
    let legacy = asset_unlock(1, 9);
    tip.reject_signed.insert(expired.txid(), "bad-assetunlock-too-late");
    let mut dag = Dag::default();
    let expired = dag.add_tx(expired, &[], 3000, 100, 1);
    let fresh = dag.add_tx(fresh, &[], 2000, 100, 1);
    let legacy = dag.add_tx(legacy, &[], 1000, 100, 1);
    let pool = dag.pool();
    let mut rules = rules(&tip, &hooks, true);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!(ids, vec![fresh, legacy]);
    assert_eq!(
        log,
        vec![format!(
            "addPackageTxs: package tx {} skipped, asset unlock instance not currently minable: \
             bad-assetunlock-too-late",
            txid(&pool, expired)
        )]
    );
    let signed: Vec<_> = tip.special_calls().into_iter().filter(|(_, sigs)| *sigs).collect();
    assert_eq!(signed, vec![(txid(&pool, expired), true), (txid(&pool, fresh), true)]);
}

/// Core v24.0.2 src/node/miner.cpp:759-781, 800-805: two ProRegTx each valid at the tip but
/// sharing an operator key: the second does not apply to the list as built so far and is
/// skipped; a third that applies goes in after it, so the first one's list was committed.
#[test]
fn the_second_of_two_conflicting_pro_reg_txs_is_skipped() {
    let world = ListWorld::new();
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let first = dag.add_tx(pro_reg(11, 50, None), &[], 3000, 100, 1);
    let second = dag.add_tx(pro_reg(12, 50, None), &[], 2000, 100, 1);
    let third = dag.add_tx(pro_reg(13, 51, None), &[], 1000, 100, 1);
    let pool = dag.pool();
    let alone = |id| {
        let mut builder = world.builder();
        tip.with_list_inputs(|coins, members| {
            builder.apply_tx(
                pool.mining_entry(id).tx,
                coins,
                members,
                &mut Vec::new(),
                &mut NoTrace,
            )
        })
        .unwrap()
    };
    assert_eq!((alone(first), alone(second), alone(third)), (Ok(()), Ok(()), Ok(())));

    let mut inputs = inputs(&tip, &hooks, true);
    inputs.builder = Some(world.builder());
    let mut rules = PackageRules::new(inputs);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!(ids, vec![first, third]);
    assert_eq!(
        log,
        vec![format!(
            "addPackageTxs: package tx {} skipped, does not apply to the masternode list: {}",
            txid(&pool, second),
            "bad-protx-dup-key"
        )]
    );
    assert!(rules.finish().unwrap().is_some());
}

/// Core v24.0.2 src/node/miner.cpp:759-790: the credit pool runs after a passing list trial and
/// names the candidate (`iter`), not the failing transaction; a failed trial skips the package
/// before the credit pool.
#[test]
fn a_credit_pool_failure_after_a_passing_list_trial() {
    let world = ListWorld::new();
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let lock = dag.add_tx(asset_lock(1, 1000, false), &[], 1000, 100, 1);
    let child = dag.add_tx(ordinary(2), &[lock], 5000, 100, 1);
    let mut inputs = inputs(&tip, &hooks, true);
    inputs.builder = Some(world.builder());
    inputs.credit_pool = Some(CreditPool::default());
    let pool = dag.pool();
    let mut rules = PackageRules::new(inputs);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!(ids, vec![]);
    let skipped = format!(
        "addPackageTxs: package tx {} skipped due to credit pool state: failed-creditpool-lock-invalid",
        txid(&pool, child)
    );
    // {lock, child}, then lock alone.
    let lock_skipped =
        skipped.replace(&txid(&pool, child).to_string(), &txid(&pool, lock).to_string());
    assert_eq!(log, vec![skipped, lock_skipped]);
    assert_eq!(
        tip.list_calls(),
        3,
        "both entries of the package, then the lock, went through the trial"
    );
}

/// Core v24.0.2 src/node/miner.cpp:759-790: the trial's failure comes first.
#[test]
fn a_failed_list_trial_skips_before_the_credit_pool() {
    let world = ListWorld::new();
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let first = dag.add_tx(pro_reg(11, 50, None), &[], 9000, 100, 1);
    let conflict = dag.add_tx(pro_reg(12, 50, None), &[], 1000, 100, 1);
    dag.add_tx(asset_lock(1, 1000, false), &[conflict], 5000, 100, 1);
    let mut inputs = inputs(&tip, &hooks, true);
    inputs.builder = Some(world.builder());
    inputs.credit_pool = Some(CreditPool::default());
    let pool = dag.pool();
    let mut rules = PackageRules::new(inputs);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!(ids, vec![first]);
    let trial = format!(
        "addPackageTxs: package tx {} skipped, does not apply to the masternode list: \
         bad-protx-dup-key",
        txid(&pool, conflict)
    );
    // The lock's package (whose credit pool would fail too), then the conflict alone.
    assert_eq!(log, vec![trial; 2]);
}

/// Core v24.0.2 src/node/miner.cpp:760-762: no trial for a package without a special
/// transaction, nor without a builder (below DIP0003).
#[test]
fn the_list_trial_runs_only_for_special_packages_with_a_builder() {
    let world = ListWorld::new();
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let plain = dag.add_tx(ordinary(1), &[], 2000, 100, 1);
    let signal = dag.add_tx(mnhf(2, 5, u32::MAX, 0), &[], 1000, 100, 1);
    let pool = dag.pool();
    let mut with = inputs(&tip, &hooks, true);
    with.builder = Some(world.builder());
    let (ids, _) = select(&pool, &mut PackageRules::new(with));
    assert_eq!((ids, tip.list_calls()), (vec![plain, signal], 1));

    let (ids, _) = select(&pool, &mut rules(&tip, &hooks, true));
    assert_eq!((ids, tip.list_calls()), (vec![plain, signal], 1));
}

/// Core v24.0.2 src/node/miner.cpp:791-805: a skipped package commits nothing, neither its
/// signals nor its key changes nor its list; the next package with the same bit, or a service
/// update of the same masternode, still goes in.
#[test]
fn a_skipped_package_commits_nothing() {
    let world = ListWorld::new();
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let first = dag.add_tx(pro_reg(11, 50, None), &[], 90_000, 100, 1);
    let conflict = dag.add_tx(pro_reg(12, 50, None), &[], 1000, 100, 1);
    let signal = dag.add_tx(mnhf(2, 5, u32::MAX, 0), &[conflict], 1000, 100, 1);
    let key_change = dag.add(&[signal], 80_000, 100, 1);
    let same_bit = dag.add_tx(mnhf(3, 5, u32::MAX, 0), &[], 3000, 100, 1);
    let update = dag.add_tx(pro_up_serv(4, mn(1)), &[], 2000, 100, 1);
    let mut pool = dag.pool();
    pool.set_key_change_target(key_change, mn(1));
    let mut inputs = inputs(&tip, &hooks, true);
    inputs.builder = Some(world.builder());
    let mut rules = PackageRules::new(inputs);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!(ids, vec![first, same_bit, update]);
    let trial = format!(
        "addPackageTxs: package tx {} skipped, does not apply to the masternode list: \
         bad-protx-dup-key",
        txid(&pool, conflict)
    );
    let dup = format!(
        "addPackageTxs: package tx {} skipped due to duplicate EHF signal 5",
        txid(&pool, signal)
    );
    // key_change's package; then signal's, after same_bit's bit went in; then conflict alone.
    assert_eq!(log, vec![trial.clone(), dup, trial]);
    assert!(rules.key_changed.is_empty());
}

/// Core v24.0.2 src/node/miner.cpp:766-779: `UpdateMN` throwing in the list trial skips the
/// package with the exception's own text. Before V24, `ApplyTx` has no duplicate-key check for a
/// ProUpRegTx, so a second one moving to the first one's operator key throws.
#[test]
fn a_throwing_list_trial_skips_with_the_exception_text() {
    let world = ListWorld::new();
    let (tip, hooks) = tip_and_hooks();
    let mut dag = Dag::default();
    let first = dag.add_tx(pro_up_reg(21, mn(1), 60), &[], 3000, 100, 1);
    let second = dag.add_tx(pro_up_reg(22, mn(2), 60), &[], 2000, 100, 1);
    let third = dag.add_tx(pro_up_reg(23, mn(2), 61), &[], 1000, 100, 1);
    let pool = dag.pool();
    let mut inputs = inputs(&tip, &hooks, false);
    inputs.builder = Some(world.builder_at(false));
    let mut rules = PackageRules::new(inputs);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!(ids, vec![first, third]);
    assert_eq!(
        log,
        vec![format!(
            "addPackageTxs: package tx {} skipped, does not apply to the masternode list: \
             UpdateMN: Can't update a masternode {} with a duplicate pubKeyOperator",
            txid(&pool, second),
            mn(2)
        )]
    );
    assert!(rules.finish().unwrap().is_some());
}

/// The coins cell or LLMQ view unavailable to the list trial is a read fault, not a skip.
#[test]
fn an_unavailable_list_cell_in_the_list_trial_is_a_read_fault() {
    let world = ListWorld::new();
    let (mut tip, hooks) = tip_and_hooks();
    tip.cell_fault = true;
    let mut dag = Dag::default();
    dag.add_tx(pro_reg(11, 50, None), &[], 3000, 100, 1);
    dag.add_tx(mnhf(2, 5, u32::MAX, 0), &[], 1000, 100, 1);
    let pool = dag.pool();
    let mut inputs = inputs(&tip, &hooks, false);
    inputs.builder = Some(world.builder());
    let mut rules = PackageRules::new(inputs);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!((ids, log), (vec![], vec![]));
    assert_eq!(tip.list_calls(), 1, "nothing after the fault");
    assert_eq!(rules.finish().err(), Some(BlockError::Fault(super::CELL_FAULT.to_owned())));
}

/// A read fault in the list trial (a store error of `ApplyTx`: here the collateral coin read)
/// ends the rules, as connect's class policy has it.
#[test]
fn a_store_error_in_the_list_trial_is_a_read_fault() {
    let world = ListWorld::new();
    let (mut tip, hooks) = tip_and_hooks();
    tip.coins.set_best_block(Some(world.chain.hash(world.tip)));
    tip.coins_fault = true;
    let mut dag = Dag::default();
    dag.add_tx(pro_reg(11, 50, Some(OutPoint::new(TxHash::from([5; 32]), 1))), &[], 3000, 100, 1);
    dag.add_tx(mnhf(2, 5, u32::MAX, 0), &[], 1000, 100, 1);
    let pool = dag.pool();
    let mut inputs = inputs(&tip, &hooks, false);
    inputs.builder = Some(world.builder());
    let mut rules = PackageRules::new(inputs);
    let (ids, log) = select(&pool, &mut rules);
    assert_eq!((ids, log), (vec![], vec![]));
    assert_eq!(tip.list_calls(), 1, "nothing after the fault");
    let fault = rules.finish().err().expect("the fault");
    assert_eq!(
        fault,
        BlockError::Fault(
            "ApplyTx: evo store read failed: collateral coin: \"injected coins fault\"".to_owned()
        )
    );
}

/// The covenant's coins read failing is a read fault, not a refusal.
#[test]
fn a_coins_fault_in_the_covenant_is_a_read_fault() {
    let (mut tip, hooks) = tip_and_hooks();
    tip.coins_fault = true;
    let (pool, id) = single(ordinary(1));
    let rules = rules(&tip, &hooks, true);
    assert!(!rules.test_package_transactions(&pool, &[id]));
    assert_eq!(
        rules.finish().err(),
        Some(BlockError::Fault(
            "CheckSharedCollateralSpends: coins read: \"injected coins fault\"".to_owned()
        ))
    );
}

/// One rule flipped at a time over the same pool: only the probed transaction is left out, with
/// its rule's log line (the `TestPackageTransactions` rules log nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Toggle {
    None,
    NotFinal,
    SpecialTxRejects,
    Covenant,
    NotSafe,
    FollowsKeyChange,
    KeyChangeInPackage,
    DuplicateEhfBit,
    AssetUnlockNotMinable,
    ListTrial,
    CreditPool,
}

#[test]
fn every_rule_toggled_independently() {
    let world = ListWorld::new();
    let mut dag = Dag::default();
    let control = dag.add_tx(ordinary(9), &[], 100_000, 100, 1);
    let unlock = dag.add_tx(asset_unlock(2, 7), &[], 6000, 100, 1);
    let signal = dag.add_tx(mnhf(2, 5, 0, HEIGHT - 1), &[], 5000, 100, 1);
    let key_change = dag.add_tx(ordinary(3), &[], 4000, 100, 1);
    let update = dag.add_tx(pro_up_serv(5, mn(3)), &[key_change], 9000, 100, 1);
    let lock = dag.add_tx(asset_lock(4, 1000, true), &[], 2000, 100, 1);
    let all = [control, key_change, update, unlock, signal, lock];
    for toggle in [
        Toggle::None,
        Toggle::NotFinal,
        Toggle::SpecialTxRejects,
        Toggle::Covenant,
        Toggle::NotSafe,
        Toggle::FollowsKeyChange,
        Toggle::KeyChangeInPackage,
        Toggle::DuplicateEhfBit,
        Toggle::AssetUnlockNotMinable,
        Toggle::ListTrial,
        Toggle::CreditPool,
    ] {
        let mut pool = dag.pool();
        if toggle == Toggle::KeyChangeInPackage {
            pool.set_key_change_target(key_change, mn(3));
        }
        let (mut tip, mut hooks) = tip_and_hooks();
        hooks.enabled = true;
        for id in [control, signal, key_change, update, lock] {
            hooks.safe.insert(txid(&pool, id));
        }
        let t = |id| txid(&pool, id);
        match toggle {
            Toggle::SpecialTxRejects => {
                tip.reject.insert(t(signal), "bad-mnhf-quorum-hash");
            }
            Toggle::Covenant => {
                tip = tip.with_template_coin(OutPoint::new(TxHash::from([2; 32]), 0));
            }
            Toggle::NotSafe => {
                hooks.safe.remove(&t(signal));
            }
            Toggle::AssetUnlockNotMinable => {
                tip.reject_signed.insert(t(unlock), "bad-assetunlock-not-active-quorum");
            }
            _ => {}
        }
        let mut inputs = inputs(&tip, &hooks, true);
        match toggle {
            Toggle::NotFinal => inputs.height = HEIGHT - 1,
            Toggle::DuplicateEhfBit => inputs.signals = EhfSignals::from_iter([(5, 1)]),
            Toggle::ListTrial => inputs.builder = Some(world.builder()),
            Toggle::CreditPool => inputs.credit_pool = Some(CreditPool::default()),
            _ => {}
        }
        let mut rules = PackageRules::new(inputs);
        if toggle == Toggle::FollowsKeyChange {
            rules.key_changed.insert(mn(3));
        }
        let (ids, log) = select(&pool, &mut rules);

        let line = |id, what: &str| format!("addPackageTxs: package tx {} skipped{what}", t(id));
        let follow = follows(&pool, update, mn(3));
        let (left_out, expected_log) = match toggle {
            Toggle::None => (None, vec![]),
            Toggle::NotFinal | Toggle::SpecialTxRejects | Toggle::Covenant | Toggle::NotSafe => {
                (Some(signal), vec![])
            }
            Toggle::FollowsKeyChange | Toggle::KeyChangeInPackage => {
                (Some(update), vec![follow.clone(), follow])
            }
            Toggle::DuplicateEhfBit => {
                (Some(signal), vec![line(signal, " due to duplicate EHF signal 5")])
            }
            Toggle::AssetUnlockNotMinable => (
                Some(unlock),
                vec![line(
                    unlock,
                    ", asset unlock instance not currently minable: \
                     bad-assetunlock-not-active-quorum",
                )],
            ),
            Toggle::ListTrial => {
                let trial = line(update, ", does not apply to the masternode list: bad-protx-hash");
                (Some(update), vec![trial.clone(), trial])
            }
            Toggle::CreditPool => (
                Some(unlock),
                vec![line(unlock, " due to credit pool state: failed-creditpool-unlock-too-much")],
            ),
        };
        let mut expected: Vec<_> = all.iter().copied().filter(|id| Some(*id) != left_out).collect();
        let mut ids = ids;
        if left_out.is_some() {
            // Block order moves with what is left out; membership is the point.
            ids.sort();
            expected.sort();
        }
        assert_eq!((ids, log), (expected, expected_log), "{toggle:?}");
    }
}

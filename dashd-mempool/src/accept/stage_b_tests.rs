//! MP1a-6's stage-B tests: `AcceptToMemoryPool` end to end over a real pool, a fake V and
//! scripted InstantSend hooks. Each test cites the Core v24.0.0 lines it pins (`v:` is
//! validation.cpp, `tm:` txmempool.cpp). The minimal pairs differ in one fact and land on
//! neighbouring outcomes, so each row is shown reached in Core's precedence.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use dash_consensus::coins::Coin;
use dash_consensus::ctx::{ChainAncestry, Deployment, DeploymentPos};
use dash_consensus::hash::hash160;
use dash_consensus::primitives::decode_tx_exact;
use dash_consensus::script::ScriptFlags;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_consensus::{BlockId, Tx, TxRef};
use dash_core_vectors::cases::{BlsScheme, load_trivial};
use dash_core_vectors::read_core_text;
use dash_dml::creditpool::{CreditPool, PoolError, RangesSet};
use dash_dml::specialtx::covenant::SHARED_COLLATERAL_SCRIPT;
use dash_dml::specialtx::reasons::{BAD_SHARED_COLLATERAL_CREATE, BAD_SHARED_COLLATERAL_SPEND};
use dash_dml::testkit::{MemChain, dmn_with, operator_key, wire_key};
use dash_dml::{MnList, MnType, ProTxHash};
use dash_params::{BuriedDeployment, ChainSetup};
use dash_pkc::bls::{BlsGlobalSig, BlsLazyPk, BlsSchemeKind};
use dash_primitives::{
    AssetUnlock, OutPoint, ProTxVersion, ProUpRegTx, QuorumHash, SpecialPayload, Transaction,
    TxHash, TxIn, TxOut, TxType,
};
use dash_types::codec::BaseCodec;
use dashd_chainstate::chain::seams::AtmpCoinsCall;
use dashd_chainstate::chain::{Fatal, TipFacts};
use dashd_chainstate::connect::SpecialTxRules;
use dashd_chainstate::sigcache::ScriptCaches;

use super::finalize::is_current_for_fee_estimation;
use super::tests::{TipCoins, outpoint, params};
use super::{
    AcceptArgs, AcceptChain, AcceptPool, AcceptSignals, SpecialTxChain, StatelessRecord,
    accept_to_memory_pool,
};
use crate::options::MemPoolOptions;
use crate::pool::{EntryMetadata, Mempool, PendingEvent, PoolState};
use crate::testkit::FakeIsHooks;
use crate::types::{
    IsConflict, MemPoolRemovalReason, MempoolAcceptResult, MempoolAcceptResultType,
};

const OP_TRUE: u8 = 0x51;
const OP_NOP1: u8 = 0xb0;
const OP_CHECKMULTISIG: u8 = 0xae;
const TIP: u32 = 200;
const ACCEPT_TIME: i64 = 1_700_000_000;

/// `OP_HASH160 <Hash160(OP_TRUE)> OP_EQUAL`: standard, spent by [`SIG_TRUE`].
fn p2sh_true() -> Vec<u8> {
    [&[0xa9, 0x14][..], &hash160(&[OP_TRUE]), &[0x87]].concat()
}

/// The scriptSig pushing the redeem script `OP_TRUE`.
const SIG_TRUE: [u8; 2] = [0x01, OP_TRUE];

fn tx_of(
    version: i16,
    tx_type: TxType,
    inputs: &[(OutPoint, &[u8], u32)],
    outputs: &[(i64, &[u8])],
    lock_time: u32,
    payload: Vec<u8>,
) -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        version,
        tx_type,
        inputs.iter().map(|&(p, sig, seq)| TxIn::new(p, sig.to_vec().into(), seq)).collect(),
        outputs.iter().map(|&(v, script)| TxOut::from_duffs(v, script.to_vec().into())).collect(),
        lock_time,
        payload,
    )))
}

/// A version-2 spend of P2SH(`OP_TRUE`) inputs paying P2SH(`OP_TRUE`) outputs.
fn pay(inputs: &[OutPoint], values: &[i64]) -> TxRef {
    let script = p2sh_true();
    let ins: Vec<_> = inputs.iter().map(|&p| (p, &SIG_TRUE[..], u32::MAX)).collect();
    let outs: Vec<_> = values.iter().map(|&v| (v, &script[..])).collect();
    tx_of(2, TxType::Spend, &ins, &outs, 0, vec![])
}

fn out(tx: &TxRef, n: u32) -> OutPoint {
    OutPoint::new(tx.txid(), n)
}

/// An asset unlock of `version` (2 has a stable txid) paying 10,000 to P2SH(`OP_TRUE`), with a
/// 1,000-duff fee and `sig` as its signature bytes.
fn unlock(version: u8, index: u64, requested_height: u32, sig: u8) -> TxRef {
    let payload = AssetUnlock::new(
        version,
        index,
        1_000,
        requested_height,
        QuorumHash::from([3; 32]),
        BlsGlobalSig::decode(&[sig; 96]).expect("decodes"),
    );
    let mut bytes = Vec::new();
    payload.encode(&mut bytes);
    tx_of(3, TxType::AssetUnlock, &[], &[(10_000, &p2sh_true())], 0, bytes)
}

fn fixture(kind: &str) -> TxRef {
    let rows =
        load_trivial("trivially_valid.json", &read_core_text("src/test/data/trivially_valid.json"))
            .expect("the pinned Core vectors load");
    let row = rows
        .into_iter()
        .find(|row| row.tx_type == kind && row.scheme == BlsScheme::Basic)
        .expect("a valid fixture");
    Arc::new(decode_tx_exact(&row.tx).expect("fixture decodes").into_value())
}

/// A ProUpRegTx of `version` for `pro` setting operator `key`, spending `input`.
fn registrar(version: ProTxVersion, pro: TxHash, key: BlsLazyPk, input: OutPoint) -> TxRef {
    let template = fixture("proupregtx");
    let Some(Ok(decoded)) = template.payload() else {
        panic!("registrar fixture has a payload");
    };
    let SpecialPayload::ProviderUpdateRegistrar(p) = decoded.payload() else {
        panic!("registrar fixture has the wrong payload");
    };
    let p = ProUpRegTx::new(
        version,
        pro,
        p.mode(),
        wire_key(&key),
        p.key_id_voting(),
        p.script_payout().clone(),
        p.payouts().clone(),
        p.inputs_hash(),
    );
    let mut payload = Vec::new();
    p.encode(&mut payload);
    tx_of(
        3,
        TxType::ProviderUpdateRegistrar,
        &[(input, &SIG_TRUE[..], u32::MAX)],
        &[(1_000, &p2sh_true())],
        0,
        payload,
    )
}

/// A list of regular masternodes `(n, proTxHash, operator key)`.
fn list_of(mns: &[(u8, TxHash, BlsLazyPk)]) -> MnList {
    let mut list = MnList::scratch();
    for (n, pro, key) in mns {
        list.insert_for_tests(dmn_with(*n, MnType::Regular, |dmn, cold| {
            dmn.pro_tx_hash = ProTxHash::from(*pro.as_bytes());
            cold.version = 2;
            cold.pub_key_operator = key.clone();
        }));
    }
    list
}

/// V for one admission: a 200-block chain, CoinsTip, the deployments named active, and
/// `CheckSpecialTx` scripted per txid (pass otherwise), each call's txid and `pindexPrev`
/// height recorded.
struct FakeChain {
    chain: MemChain,
    tip: BlockId,
    coins: TipCoins,
    active: HashSet<Deployment>,
    special: BTreeMap<TxHash, TxValidationError>,
    special_calls: Vec<(TxHash, u32)>,
    list: Arc<MnList>,
    credit_pool: Option<Arc<dash_dml::creditpool::PoolState>>,
}

impl FakeChain {
    fn new() -> FakeChain {
        let mut chain = MemChain::new();
        let tip = chain.extend(chain.genesis(), TIP);
        let best = chain.hash(tip);
        FakeChain {
            chain,
            tip,
            coins: TipCoins::new(best, &[]),
            active: HashSet::from([
                Deployment::Buried(BuriedDeployment::Dip0001),
                Deployment::Buried(BuriedDeployment::Dip0003),
            ]),
            special: BTreeMap::new(),
            special_calls: Vec::new(),
            list: Arc::new(MnList::default_pre_dip3()),
            credit_pool: None,
        }
    }
}

impl TipFacts for FakeChain {
    fn tip(&self) -> Option<BlockId> {
        Some(self.tip)
    }
    fn ancestry(&self) -> &dyn ChainAncestry {
        &self.chain
    }
}

impl SpecialTxChain for FakeChain {
    fn check_special_tx(
        &mut self,
        tx: &Tx,
        pindex: BlockId,
        _: SpecialTxRules,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        self.special_calls.push((tx.txid(), self.chain.header(pindex).height));
        Ok(self.special.get(&tx.txid()).map_or(Ok(()), |reject| Err(reject.clone())))
    }
}

impl AcceptChain for FakeChain {
    fn params(&self) -> &ChainSetup {
        params()
    }
    fn deployment_active_after(&self, deployment: Deployment) -> Result<bool, Fatal> {
        Ok(self.active.contains(&deployment))
    }
    fn special_tx_rules(&self) -> Result<SpecialTxRules, Fatal> {
        Ok(SpecialTxRules {
            v24: true,
            ..Default::default()
        })
    }
    fn block_script_flags(&self) -> Option<ScriptFlags> {
        Some(ScriptFlags::MANDATORY_V24_0)
    }
    fn mn_list(&self) -> &Arc<MnList> {
        &self.list
    }
    fn credit_pool(&self) -> Option<&Arc<dash_dml::creditpool::PoolState>> {
        self.credit_pool.as_ref()
    }
    fn script_caches(&self) -> Option<Arc<ScriptCaches>> {
        None
    }
    fn is_current_for_fee_estimation(&self) -> bool {
        false
    }
    fn with_atmp_coins(&mut self, f: &mut AtmpCoinsCall<'_>) -> Result<Option<()>, Fatal> {
        f(&mut self.coins).map(|()| Some(()))
    }
}

/// A recorded validation-queue event.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ev {
    Removed(TxHash, MemPoolRemovalReason, u64),
    Added(TxHash, TxHash, i64, u64),
    DoubleSpend(TxHash, TxHash),
}

struct Recorder<'a>(&'a mut Vec<Ev>);

impl AcceptSignals for Recorder<'_> {
    fn removed(&mut self, event: PendingEvent) {
        let PendingEvent::TransactionRemovedFromMempool {
            tx,
            reason,
            sequence,
        } = event;
        self.0.push(Ev::Removed(tx.txid(), reason, sequence));
    }
    fn added(&mut self, tx: TxRef, accept_time: i64, sequence: u64) {
        self.0.push(Ev::Added(tx.txid(), tx.instance_hash(), accept_time, sequence));
    }
    fn double_spend_attempt(&mut self, tx: TxRef, conflict: TxRef) {
        self.0.push(Ev::DoubleSpend(tx.txid(), conflict.txid()));
    }
}

/// MP1a-8's `AcceptPool` stand-in: the real pool's write guard and a queue recorder.
struct TestPool {
    pool: Mempool,
    events: RefCell<Vec<Ev>>,
}

impl AcceptPool for TestPool {
    fn with_write(&self, f: &mut dyn FnMut(&mut PoolState, &mut dyn AcceptSignals)) {
        let mut state = self.pool.write();
        f(&mut state, &mut Recorder(&mut self.events.borrow_mut()));
    }
}

struct Fx {
    pool: TestPool,
    chain: FakeChain,
}

const ARGS: AcceptArgs = AcceptArgs {
    accept_time: ACCEPT_TIME,
    bypass_limits: false,
    test_accept: false,
};
const TEST_ACCEPT: AcceptArgs = AcceptArgs {
    test_accept: true,
    ..ARGS
};

impl Fx {
    fn new() -> Fx {
        Fx::with_options(MemPoolOptions::default())
    }

    fn nonstandard() -> Fx {
        Fx::with_options(MemPoolOptions {
            require_standard: false,
            ..MemPoolOptions::default()
        })
    }

    fn with_options(options: MemPoolOptions) -> Fx {
        Fx {
            pool: TestPool {
                pool: Mempool::new(options),
                events: RefCell::new(Vec::new()),
            },
            chain: FakeChain::new(),
        }
    }

    fn hooks(&self, hooks: FakeIsHooks) {
        self.pool.pool.set_is_hooks(Arc::new(hooks));
    }

    /// A confirmed coin of `value` at `height` paying `script`.
    fn fund_with(&mut self, byte: u8, value: i64, height: u32, script: &[u8]) -> OutPoint {
        let op = outpoint(byte, 0);
        self.chain.coins.coins.insert(op, Coin::new(value, script, height, false));
        op
    }

    fn fund(&mut self, byte: u8) -> OutPoint {
        self.fund_with(byte, 100_000, 100, &p2sh_true())
    }

    /// `AcceptToMemoryPool`, the reply run as MP1a-4's dispatch runs it: the result and the
    /// completion's uncache list.
    fn submit(&mut self, tx: &TxRef, args: AcceptArgs) -> (MempoolAcceptResult, Vec<OutPoint>) {
        let record = StatelessRecord::new(tx, self.pool.pool.read().options());
        let got = Arc::new(Mutex::new(None));
        let sink = got.clone();
        let completion = accept_to_memory_pool(
            &self.pool,
            &mut self.chain,
            tx,
            &record,
            args,
            Box::new(move |result| *sink.lock().expect("unpoisoned") = Some(result)),
        )
        .expect("no fault");
        assert!(got.lock().expect("unpoisoned").is_none(), "the reply waits for the dispatch");
        (completion.reply)();
        let result = got.lock().expect("unpoisoned").take().expect("replied");
        (result, completion.uncache)
    }

    fn verdict_with(&mut self, tx: &TxRef, args: AcceptArgs) -> (TxValidationResult, String) {
        let (result, _) = self.submit(tx, args);
        (result.validation_result, result.reason)
    }

    fn verdict(&mut self, tx: &TxRef) -> (TxValidationResult, String) {
        self.verdict_with(tx, ARGS)
    }

    fn accept(&mut self, tx: &TxRef) {
        let (result, _) = self.submit(tx, ARGS);
        assert_eq!(result.result_type, MempoolAcceptResultType::Valid, "{result:?}");
    }

    fn held(&self, tx: &TxRef) -> bool {
        self.pool.pool.read().exists(&tx.txid())
    }

    fn take_events(&self) -> Vec<Ev> {
        std::mem::take(&mut self.pool.events.borrow_mut())
    }
}

fn ok() -> (TxValidationResult, String) {
    (TxValidationResult::Unset, String::new())
}

fn no(result: TxValidationResult, reason: &str) -> (TxValidationResult, String) {
    (result, reason.to_owned())
}

/// `AcceptSingleTransaction` (`v:1428-1466`) and `AcceptToMemoryPool` (`v:1743-1754`): an
/// admission reports `Success(vsize, base fees, CFeeRate(modified fees, vsize))`, queues
/// `TransactionAddedToMempool` with the next sequence and uncaches nothing; `test_accept` reports
/// the same, adds nothing, raises no event, and uncaches the inputs it cached.
#[test]
fn admission_and_test_accept() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let tx = pay(&[coin], &[90_000]);
    assert_eq!(tx.size(), 85);

    let (result, uncache) = fx.submit(&tx, TEST_ACCEPT);
    let expected = MempoolAcceptResult {
        result_type: MempoolAcceptResultType::Valid,
        validation_result: TxValidationResult::Unset,
        reason: String::new(),
        debug_message: String::new(),
        vsize: Some(85),
        base_fee: Some(10_000),
        effective_feerate: Some(10_000 * 1_000 / 85),
        txids_fee_calculations: Some(vec![tx.txid()]),
    };
    assert_eq!(result, expected);
    assert_eq!(uncache, vec![coin]);
    assert!(!fx.held(&tx));
    assert_eq!(fx.take_events(), vec![]);

    let sequence = fx.pool.pool.read().sequence();
    let (result, uncache) = fx.submit(&tx, ARGS);
    assert_eq!(result, expected);
    assert_eq!(uncache, vec![]);
    assert!(fx.held(&tx));
    assert_eq!(
        fx.take_events(),
        vec![Ev::Added(tx.txid(), tx.instance_hash(), ACCEPT_TIME, sequence)]
    );
    let entry_height = fx.pool.pool.read().get_iter(&tx.txid()).map(|e| e.metadata().height);
    assert_eq!(entry_height, Some(TIP));
}

/// Rows 1-2 (`v:857-864`): `CheckTransaction` first, then `ContextualCheckTransaction` at the tip,
/// which precedes `qc-not-allowed` (`v:866-869`) and the policy rows (`v:877-887`). An oversize
/// quorum commitment, or an oversize standard spend, fails contextually while DIP0001 is active
/// after the tip, and at the later row without it.
#[test]
fn rows_1_2_check_transaction_then_contextual() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let dup = pay(&[coin, coin], &[90_000]);
    assert_eq!(fx.verdict(&dup), no(TxValidationResult::Consensus, "bad-txns-inputs-duplicate"));

    let big = vec![OP_TRUE; 100_001];
    let qc = tx_of(3, TxType::QuorumCommitment, &[], &[(0, &big)], 0, vec![]);
    let oversize = pay(&[coin], &[90_000]).to_mutable();
    let mut outputs = oversize.outputs().clone();
    outputs.push(TxOut::from_duffs(1_000, big.clone().into()));
    let oversize = Arc::new(Tx::new(Transaction::new(
        2,
        TxType::Spend,
        oversize.inputs().clone(),
        outputs,
        0,
        vec![],
    )));
    let dup_oversize = Arc::new(Tx::new(Transaction::new(
        2,
        TxType::Spend,
        dup.inputs().to_vec(),
        oversize.outputs().to_vec(),
        0,
        vec![],
    )));
    assert_eq!(fx.verdict(&qc), no(TxValidationResult::Consensus, "bad-txns-oversize"));
    assert_eq!(fx.verdict(&oversize), no(TxValidationResult::Consensus, "bad-txns-oversize"));
    assert_eq!(
        fx.verdict(&dup_oversize),
        no(TxValidationResult::Consensus, "bad-txns-inputs-duplicate")
    );

    fx.chain.active.remove(&Deployment::Buried(BuriedDeployment::Dip0001));
    assert_eq!(fx.verdict(&qc), no(TxValidationResult::Consensus, "qc-not-allowed"));
    assert_eq!(fx.verdict(&oversize), no(TxValidationResult::NotStandard, "tx-size"));
}

/// Rows 3-4 (`v:866-873`): a quorum commitment is `qc-not-allowed`, a coinbase is `coinbase`,
/// both `TX_CONSENSUS`, before any policy row; with DIP0003 inactive a version-3 commitment is
/// still refused at row 3.
#[test]
fn rows_3_4_quorum_commitment_and_coinbase() {
    let mut fx = Fx::new();
    let qc = tx_of(3, TxType::QuorumCommitment, &[], &[], 0, vec![0; 100]);
    assert_eq!(fx.verdict(&qc), no(TxValidationResult::Consensus, "qc-not-allowed"));
    fx.chain.active.remove(&Deployment::Buried(BuriedDeployment::Dip0003));
    assert_eq!(fx.verdict(&qc), no(TxValidationResult::Consensus, "qc-not-allowed"));

    let script = p2sh_true();
    let coinbase = tx_of(
        4,
        TxType::Spend,
        &[(OutPoint::new(TxHash::from([0; 32]), u32::MAX), &[1, 1][..], u32::MAX)],
        &[(50, &script)],
        0,
        vec![],
    );
    assert!(coinbase.is_coinbase());
    assert_eq!(fx.verdict(&coinbase), no(TxValidationResult::Consensus, "coinbase"));
}

/// Rows 5-7 (`v:877-887`): with `require_standard`, `IsStandardTx`'s and `IsStandardSpecialTx`'s
/// reasons (`TX_NOT_STANDARD`); without it both are skipped; `tx-size-small` (< 65 bytes) is
/// unconditional and comes after them.
#[test]
fn rows_5_7_standardness_then_minimum_size() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let mut v4 = pay(&[coin], &[90_000]).to_mutable();
    v4 = Transaction::new(4, TxType::Spend, v4.inputs().clone(), v4.outputs().clone(), 0, vec![]);
    let v4 = Arc::new(Tx::new(v4));
    assert_eq!(fx.verdict(&v4), no(TxValidationResult::NotStandard, "version"));

    let script = p2sh_true();
    let many: Vec<_> = (0..101).map(|n| (outpoint(9, n), &SIG_TRUE[..], u32::MAX)).collect();
    let lock = tx_of(3, TxType::AssetLock, &many, &[(1_000, &script)], 0, vec![]);
    assert_eq!(
        fx.verdict(&lock),
        no(TxValidationResult::NotStandard, "assetlocktx-too-many-inputs")
    );

    let small =
        tx_of(2, TxType::Spend, &[(coin, &[], u32::MAX)], &[(90_000, &[OP_TRUE])], 0, vec![]);
    assert_eq!(small.size(), 61);
    assert_eq!(fx.verdict(&small), no(TxValidationResult::NotStandard, "scriptpubkey"));
    let small_v4 =
        tx_of(4, TxType::Spend, &[(coin, &[], u32::MAX)], &[(90_000, &[OP_TRUE])], 0, vec![]);
    assert_eq!(fx.verdict(&small_v4), no(TxValidationResult::NotStandard, "version"));

    let mut fx = Fx::nonstandard();
    fx.fund(1);
    assert_eq!(
        fx.verdict(&lock),
        no(TxValidationResult::MissingInputs, "bad-txns-inputs-missingorspent")
    );
    assert_eq!(fx.verdict(&small), no(TxValidationResult::NotStandard, "tx-size-small"));
    assert_eq!(fx.verdict(&small_v4), no(TxValidationResult::NotStandard, "tx-size-small"));
    assert_eq!(fx.verdict(&v4), ok());
}

/// Row 8 (`v:892-894`, `CheckFinalTxAtTip` `v:205-225`): a lock time at the next block's height
/// is `non-final` (`TX_PREMATURE_SPEND`), one below it is final. Row 9 (`v:896-900`): a held txid
/// is `txn-already-in-mempool` (`TX_CONFLICT`), and finality is checked first.
#[test]
fn rows_8_9_finality_then_already_held() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let script = p2sh_true();
    let at = |lock_time| {
        tx_of(
            2,
            TxType::Spend,
            &[(coin, &SIG_TRUE[..], 0)],
            &[(90_000, &script[..])],
            lock_time,
            vec![],
        )
    };
    assert_eq!(fx.verdict(&at(TIP + 1)), no(TxValidationResult::PrematureSpend, "non-final"));
    let final_tx = at(TIP);
    fx.accept(&final_tx);
    assert_eq!(fx.verdict(&final_tx), no(TxValidationResult::Conflict, "txn-already-in-mempool"));
}

/// Row 10 (`v:902-910`): a conflicting IS lock is `tx-txlock-conflict` (`TX_CONFLICT_LOCK`),
/// with `NotifyInstantSendDoubleSpendAttempt` only when the locked transaction is found (the
/// mempool here; the txindex half joins with IX1). A held txid is still row 9 first.
#[test]
fn row_10_conflicting_lock_notifies_when_the_locked_tx_is_held() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let locked = pay(&[coin], &[90_000]);
    fx.accept(&locked);
    fx.take_events();
    let spender = pay(&[coin], &[80_000]);
    let unknown = pay(&[fx.fund(2)], &[80_000]);
    let lock = |txid| IsConflict {
        lock_hash: dash_num::Hash256::from([7; 32]),
        txid,
    };
    fx.hooks(FakeIsHooks {
        enabled: true,
        conflicts: BTreeMap::from([
            (spender.txid(), lock(locked.txid())),
            (unknown.txid(), lock(TxHash::from([8; 32]))),
            (locked.txid(), lock(locked.txid())),
        ]),
        ..FakeIsHooks::default()
    });
    assert_eq!(fx.verdict(&spender), no(TxValidationResult::ConflictLock, "tx-txlock-conflict"));
    assert_eq!(fx.take_events(), vec![Ev::DoubleSpend(spender.txid(), locked.txid())]);
    assert_eq!(fx.verdict(&unknown), no(TxValidationResult::ConflictLock, "tx-txlock-conflict"));
    assert_eq!(fx.take_events(), vec![]);
    assert_eq!(fx.verdict(&locked), no(TxValidationResult::Conflict, "txn-already-in-mempool"));
}

/// Row 11 (`v:913-928`): without IS waiting for it, a spend of an in-pool outpoint is
/// `txn-mempool-conflict`. While IS waits for the txid, `removeConflicts` evicts the other
/// spender and its descendants (CONFLICT events, `tm:957-989`); those removals stand when a later
/// row rejects the transaction, and under `test_accept`.
#[test]
fn row_11_waiting_branch_evicts_conflicts_and_removals_persist() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let held = pay(&[coin], &[90_000]);
    let child = pay(&[out(&held, 0)], &[80_000]);
    let spender = pay(&[coin], &[85_000]);
    let cheap = pay(&[coin], &[99_990]);
    let tested = pay(&[coin], &[70_000]);
    let refill = |fx: &mut Fx| {
        fx.accept(&held);
        fx.accept(&child);
        fx.take_events();
    };
    refill(&mut fx);
    assert_eq!(fx.verdict(&spender), no(TxValidationResult::Conflict, "txn-mempool-conflict"));
    assert!(fx.held(&held) && fx.held(&child));

    fx.hooks(FakeIsHooks {
        enabled: true,
        waiting: [spender.txid(), cheap.txid(), tested.txid()].into(),
        ..FakeIsHooks::default()
    });
    let seq = fx.pool.pool.read().sequence();
    assert_eq!(fx.verdict(&spender), ok());
    let events = fx.take_events();
    assert_eq!(events.len(), 3);
    let removed: HashSet<_> = events[..2]
        .iter()
        .map(|e| match e {
            Ev::Removed(txid, MemPoolRemovalReason::Conflict, s) if *s < seq + 2 => *txid,
            other => panic!("not a conflict removal: {other:?}"),
        })
        .collect();
    assert_eq!(removed, HashSet::from([held.txid(), child.txid()]));
    assert_eq!(events[2], Ev::Added(spender.txid(), spender.instance_hash(), ACCEPT_TIME, seq + 2));

    // The evictions stand when min relay fee (row 22) refuses the transaction.
    let mut fx2 = Fx::new();
    fx2.fund(1);
    refill(&mut fx2);
    fx2.hooks(FakeIsHooks {
        enabled: true,
        waiting: [cheap.txid(), tested.txid()].into(),
        ..FakeIsHooks::default()
    });
    assert_eq!(fx2.verdict(&cheap), no(TxValidationResult::MempoolPolicy, "min relay fee not met"));
    assert!(!fx2.held(&held) && !fx2.held(&child));
    assert_eq!(fx2.take_events().len(), 2);

    // And under test_accept.
    refill(&mut fx2);
    let (result, _) = fx2.submit(&tested, TEST_ACCEPT);
    assert_eq!(result.result_type, MempoolAcceptResultType::Valid);
    assert!(!fx2.held(&held) && !fx2.held(&child) && !fx2.held(&tested));
    assert!(fx2.take_events().iter().all(|e| matches!(e, Ev::Removed(..))));
}

/// Row 11's ProTx and asset-unlock removals (`v:913-917`; `tm:1212-1316`, `tm:838-850`): while IS
/// waits for a ProUpRegTx changing an operator key, the pending ProUpServTx it supersedes is
/// evicted (MPIX-14), and stays evicted when `CheckSpecialTx` (row 25) refuses it; while IS waits
/// for an asset unlock, the held claimant of its index is evicted before row 13, so a staler
/// instance is admitted that row 13 would otherwise refuse.
#[test]
fn row_11_waiting_branch_protx_and_asset_unlock_removals() {
    let service = fixture("proupservtx");
    let Some(Ok(decoded)) = service.payload() else {
        panic!("service fixture payload");
    };
    let SpecialPayload::ProviderUpdateService(p) = decoded.payload() else {
        panic!("service fixture type");
    };
    let pro = p.pro_tx_hash();
    let mut fx = Fx::nonstandard();
    fx.chain.list = Arc::new(list_of(&[(1, pro, operator_key(9, BlsSchemeKind::Basic))]));
    let coin = fx.fund(1);
    let change =
        registrar(ProTxVersion::BasicBls, pro, operator_key(5, BlsSchemeKind::Basic), coin);
    {
        let mut state = fx.pool.pool.write();
        let entry = state.new_entry(service.clone(), EntryMetadata::default());
        state.add_unchecked_at(entry, &fx.chain.list);
    }
    fx.hooks(FakeIsHooks {
        enabled: true,
        waiting: [change.txid()].into(),
        ..FakeIsHooks::default()
    });
    fx.chain.special.insert(
        change.txid(),
        TxValidationError::new(TxValidationResult::BadSpecial, "bad-protx-test"),
    );
    assert_eq!(fx.verdict(&change), no(TxValidationResult::BadSpecial, "bad-protx-test"));
    assert!(!fx.held(&service));
    assert!(matches!(
        fx.take_events()[..],
        [Ev::Removed(txid, MemPoolRemovalReason::Conflict, _)] if txid == service.txid()
    ));

    let mut fx = Fx::new();
    let fresher = unlock(1, 7, 150, 1);
    let staler = unlock(1, 7, 140, 2);
    fx.accept(&fresher);
    assert_eq!(fx.verdict(&staler), no(TxValidationResult::Conflict, "assetunlock-stale-instance"));
    fx.hooks(FakeIsHooks {
        enabled: true,
        waiting: [staler.txid()].into(),
        ..FakeIsHooks::default()
    });
    assert_eq!(fx.verdict(&staler), ok());
    assert!(!fx.held(&fresher) && fx.held(&staler));
}

fn credit_pool(indexes: &[u64]) -> Option<Arc<dash_dml::creditpool::PoolState>> {
    let mut set = RangesSet::default();
    for index in indexes {
        set.add(*index);
    }
    Some(Arc::new(Ok(CreditPool {
        locked: 0,
        current_limit: 0,
        lately_unlocked: 0,
        indexes: set,
    })))
}

/// Rows 12-13 (`v:939-968`) and `Finalize`'s eviction (`v:1258-1260`): an index the tip's credit
/// pool holds is `txn-already-known` (before row 13); a failed pool build is
/// `failed-getcreditpool` (`TX_BAD_SPECIAL`); a held claimant of the index refuses an equal or
/// lower `requestedHeight` (`assetunlock-stale-instance`), while a higher one (here a version-2
/// re-signing of a version-1 instance) is admitted and evicts the held claimant and its
/// descendants. Core's `EvoDbInconsistencyError` branch (`v:945-947`) has no producer here (V
/// publishes the tip's pool already built; MP1a-5 deviation 2).
#[test]
fn rows_12_13_asset_unlock_index_claims() {
    let mut fx = Fx::new();
    let held = unlock(1, 7, 150, 1);
    fx.accept(&held);
    let child = pay(&[out(&held, 0)], &[9_000]);
    fx.accept(&child);
    fx.take_events();

    fx.chain.credit_pool = credit_pool(&[7]);
    let equal = unlock(1, 7, 150, 2);
    assert_eq!(fx.verdict(&equal), no(TxValidationResult::Conflict, "txn-already-known"));
    fx.chain.credit_pool = Some(Arc::new(Err(PoolError::PrevLocked)));
    assert_eq!(fx.verdict(&equal), no(TxValidationResult::BadSpecial, "failed-getcreditpool"));
    fx.chain.credit_pool = credit_pool(&[8]);
    assert_eq!(fx.verdict(&equal), no(TxValidationResult::Conflict, "assetunlock-stale-instance"));
    assert_eq!(
        fx.verdict(&unlock(1, 7, 149, 3)),
        no(TxValidationResult::Conflict, "assetunlock-stale-instance")
    );

    let fresher = unlock(2, 7, 151, 4);
    assert!(fresher.is_asset_unlock_with_stable_txid());
    assert_eq!(fx.verdict(&fresher), ok());
    assert!(!fx.held(&held) && !fx.held(&child) && fx.held(&fresher));
    let events = fx.take_events();
    let removed: HashSet<_> = events
        .iter()
        .filter_map(|e| match e {
            Ev::Removed(txid, MemPoolRemovalReason::Conflict, _) => Some(*txid),
            _ => None,
        })
        .collect();
    assert_eq!(removed, HashSet::from([held.txid(), child.txid()]));
    assert!(matches!(events.last(), Some(Ev::Added(txid, ..)) if *txid == fresher.txid()));
    assert_eq!(fx.pool.pool.read().asset_unlock_txids_by_index(7), vec![fresher.txid()]);
}

/// `TryAssetUnlockRefresh` (`v:1376-1426`, run first at `v:1434`): a held stable-txid unlock's
/// identical instance is `txn-already-in-mempool` without `CheckSpecialTx`; a fresher instance
/// whose window holds the tip passes `CheckSpecialTx` at the tip, replaces the held instance in
/// place and reports the held entry's size and fee with an added event (`v:1418-1424`), but not
/// under `test_accept`; one not fresher, or whose window misses the tip, is
/// `assetunlock-stale-instance`. Refresh versus normal admission: the refresh path skips
/// `PreChecks`, so a credit pool naming the index refuses a new claimant (row 12) yet not the
/// refresh of a held one.
#[test]
fn refresh_versus_normal_admission() {
    let mut fx = Fx::new();
    // Its window [140, 187] closed below the tip: CheckSpecialTxForMempool checks it at
    // chain[140] (v:827-831).
    let held = unlock(2, 7, 140, 1);
    fx.accept(&held);
    assert_eq!(fx.chain.special_calls, vec![(held.txid(), 140)]);
    fx.chain.special_calls.clear();
    fx.take_events();

    assert_eq!(fx.verdict(&held), no(TxValidationResult::Conflict, "txn-already-in-mempool"));
    for requested in [140, 139, 152, TIP + 1] {
        let instance = unlock(2, 7, requested, 2);
        assert_eq!(instance.txid(), held.txid());
        assert_eq!(
            fx.verdict(&instance),
            no(TxValidationResult::Conflict, "assetunlock-stale-instance"),
            "requested {requested}"
        );
    }
    assert_eq!(fx.chain.special_calls, vec![]);

    let refused = unlock(2, 7, 153, 3);
    fx.chain.special.insert(
        refused.txid(),
        TxValidationError::new(TxValidationResult::BadSpecial, "bad-assetunlock-too-late"),
    );
    assert_eq!(
        fx.verdict(&refused),
        no(TxValidationResult::BadSpecial, "bad-assetunlock-too-late")
    );
    fx.chain.special.clear();
    fx.chain.special_calls.clear();

    fx.chain.credit_pool = credit_pool(&[7]);
    let (held_vsize, held_fee) = {
        let state = fx.pool.pool.read();
        let entry = state.get_iter(&held.txid()).expect("held");
        (entry.vsize() as i64, entry.metadata().fee)
    };
    let tested = unlock(2, 7, 160, 4);
    let (result, _) = fx.submit(&tested, TEST_ACCEPT);
    assert_eq!((result.vsize, result.base_fee), (Some(held_vsize), Some(held_fee)));
    assert_eq!(fx.take_events(), vec![]);
    let instance = |fx: &Fx| fx.pool.pool.read().get(&held.txid()).map(|tx| tx.instance_hash());
    assert_eq!(instance(&fx), Some(held.instance_hash()));

    let seq = fx.pool.pool.read().sequence();
    let (result, uncache) = fx.submit(&tested, ARGS);
    assert_eq!(result.result_type, MempoolAcceptResultType::Valid);
    assert_eq!(uncache, vec![]);
    assert_eq!(instance(&fx), Some(tested.instance_hash()));
    assert_eq!(
        fx.take_events(),
        vec![Ev::Added(held.txid(), tested.instance_hash(), ACCEPT_TIME, seq)]
    );
    assert_eq!(fx.chain.special_calls, vec![(held.txid(), TIP), (held.txid(), TIP)]);

    let other = unlock(2, 7, 170, 5);
    let mut fx = Fx::new();
    fx.chain.credit_pool = credit_pool(&[7]);
    assert_eq!(fx.verdict(&other), no(TxValidationResult::Conflict, "txn-already-known"));
}

/// Row 14 (`v:972-990`): a missing input is `bad-txns-inputs-missingorspent`
/// (`TX_MISSING_INPUTS`), or `txn-already-known` when CoinsTip has one of the transaction's own
/// outputs cached; the inputs CoinsTip had not cached are returned for uncaching
/// (`v:1747-1754`).
#[test]
fn row_14_missing_inputs() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let tx = pay(&[coin, outpoint(2, 0)], &[90_000]);
    let (result, uncache) = fx.submit(&tx, ARGS);
    assert_eq!(
        (result.validation_result, result.reason.as_str()),
        (TxValidationResult::MissingInputs, "bad-txns-inputs-missingorspent")
    );
    assert_eq!(uncache, vec![coin, outpoint(2, 0)]);
    fx.chain.coins.cached.insert(out(&tx, 0));
    assert_eq!(fx.verdict(&tx), no(TxValidationResult::Conflict, "txn-already-known"));
}

/// Row 16 (`v:1010-1012`): a BIP68 lock the next block does not satisfy is `non-BIP68-final`
/// (`TX_PREMATURE_SPEND`); the same lock on an older coin passes.
#[test]
fn row_16_sequence_locks() {
    let mut fx = Fx::new();
    let script = p2sh_true();
    let young = fx.fund_with(1, 100_000, TIP, &script);
    let old = fx.fund_with(2, 100_000, TIP - 5, &script);
    let locked = |coin| {
        tx_of(2, TxType::Spend, &[(coin, &SIG_TRUE[..], 5)], &[(90_000, &script[..])], 0, vec![])
    };
    assert_eq!(
        fx.verdict(&locked(young)),
        no(TxValidationResult::PrematureSpend, "non-BIP68-final")
    );
    assert_eq!(fx.verdict(&locked(old)), ok());
}

/// Row 17 (`v:1014-1018`): `Consensus::CheckTxInputs` at tip + 1, its reject unchanged.
#[test]
fn row_17_check_tx_inputs() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    assert_eq!(
        fx.verdict(&pay(&[coin], &[100_001])),
        no(TxValidationResult::Consensus, "bad-txns-in-belowout")
    );
    assert_eq!(
        fx.verdict(&pay(&[coin], &[100_000])),
        no(TxValidationResult::MempoolPolicy, "min relay fee not met")
    );
}

/// Row 18 (`v:1019-1029`): with V24 active after the tip, `CheckSharedCollateralSpends`, then
/// `CheckSharedCollateralTemplateOutputs`; without V24 neither runs.
#[test]
fn row_18_shared_collateral_after_v24() {
    let mut fx = Fx::nonstandard();
    let template = fx.fund_with(1, 100_000, 100, &SHARED_COLLATERAL_SCRIPT);
    let plain = fx.fund(2);
    let script = p2sh_true();
    let spend =
        tx_of(2, TxType::Spend, &[(template, &[], u32::MAX)], &[(90_000, &script)], 0, vec![]);
    let create = tx_of(
        2,
        TxType::Spend,
        &[(plain, &SIG_TRUE[..], u32::MAX)],
        &[(90_000, &SHARED_COLLATERAL_SCRIPT)],
        0,
        vec![],
    );
    let both = tx_of(
        2,
        TxType::Spend,
        &[(template, &[], u32::MAX)],
        &[(90_000, &SHARED_COLLATERAL_SCRIPT)],
        0,
        vec![],
    );
    fx.chain.active.insert(Deployment::Versionbits(DeploymentPos::V24));
    assert_eq!(fx.verdict(&spend), no(TxValidationResult::Consensus, BAD_SHARED_COLLATERAL_SPEND));
    assert_eq!(
        fx.verdict(&create),
        no(TxValidationResult::Consensus, BAD_SHARED_COLLATERAL_CREATE)
    );
    assert_eq!(fx.verdict(&both), no(TxValidationResult::Consensus, BAD_SHARED_COLLATERAL_SPEND));
    fx.chain.active.remove(&Deployment::Versionbits(DeploymentPos::V24));
    assert_eq!(fx.verdict(&spend), ok());
    assert_eq!(fx.verdict(&create), ok());
}

/// Row 19 (`v:1032-1033`): with `require_standard`, a nonstandard prevout script is
/// `bad-txns-nonstandard-inputs` (`TX_INPUTS_NOT_STANDARD`).
#[test]
fn row_19_nonstandard_inputs() {
    let script = p2sh_true();
    for (mut fx, verdict) in [
        (Fx::new(), no(TxValidationResult::InputsNotStandard, "bad-txns-nonstandard-inputs")),
        (Fx::nonstandard(), ok()),
    ] {
        let bare = fx.fund_with(1, 100_000, 100, &[OP_TRUE]);
        let tx = tx_of(2, TxType::Spend, &[(bare, &[], u32::MAX)], &[(90_000, &script)], 0, vec![]);
        assert_eq!(fx.verdict(&tx), verdict);
    }
}

/// Rows 20-21 (`v:1036-1059`): more than 4,000 standard sigops is `bad-txns-too-many-sigops`
/// (`TX_NOT_STANDARD`, the count as debug), before the fee rows; 4,000 passes, priced by
/// `bytes_per_sigop` in the entry's vsize.
#[test]
fn row_21_too_many_sigops() {
    let mut fx = Fx::nonstandard();
    let coin = fx.fund_with(1, 1_000_000, 100, &p2sh_true());
    let multisig = |n: usize, value| {
        let script = vec![OP_CHECKMULTISIG; n];
        tx_of(
            2,
            TxType::Spend,
            &[(coin, &SIG_TRUE[..], u32::MAX)],
            &[(value, &script[..])],
            0,
            vec![],
        )
    };
    let (result, _) = fx.submit(&multisig(201, 1_000_000), ARGS);
    assert_eq!(
        (result.validation_result, result.reason.as_str(), result.debug_message.as_str()),
        (TxValidationResult::NotStandard, "bad-txns-too-many-sigops", "4020")
    );
    let (result, _) = fx.submit(&multisig(200, 100_000), ARGS);
    assert_eq!(result.result_type, MempoolAcceptResultType::Valid);
    assert_eq!(result.vsize, Some(4_000 * 20));
}

/// Rows 22-23 (`v:1060-1076`, `CheckFeeRate` `v:786-799`): a modified fee below the minimum relay
/// fee for the vsize is `min relay fee not met` (`TX_MEMPOOL_POLICY`, `"fee < min"` as debug);
/// `bypass_limits` skips it, and a `prioritisetransaction` delta counts (`ApplyDelta`,
/// `v:1039-1040`). An MNHF signal is exempt: it is admitted with no fee. `GetMinFee` is MP1b's
/// (0), so `mempool min fee not met` cannot arise in MP1a.
#[test]
fn rows_22_23_fee_floors_and_the_mnhf_exemption() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let cheap = pay(&[coin], &[99_950]);
    let (result, _) = fx.submit(&cheap, ARGS);
    assert_eq!(
        (result.validation_result, result.reason.as_str(), result.debug_message.as_str()),
        (TxValidationResult::MempoolPolicy, "min relay fee not met", "50 < 85")
    );
    let bypass = AcceptArgs {
        bypass_limits: true,
        ..ARGS
    };
    assert_eq!(
        fx.verdict_with(
            &cheap,
            AcceptArgs {
                test_accept: true,
                ..bypass
            }
        ),
        ok()
    );
    fx.pool.pool.write().prioritise_transaction(cheap.txid(), 35);
    let (result, _) = fx.submit(&cheap, TEST_ACCEPT);
    assert_eq!((result.base_fee, result.effective_feerate), (Some(50), Some(85 * 1_000 / 85)));

    let mnhf = tx_of(3, TxType::MnhfSignal, &[], &[], 0, vec![0; 130]);
    let (result, _) = fx.submit(&mnhf, ARGS);
    assert_eq!(result.result_type, MempoolAcceptResultType::Valid, "{result:?}");
    assert_eq!(result.base_fee, Some(0));
    let free = pay(&[fx.fund(2)], &[100_000]);
    assert_eq!(fx.verdict(&free), no(TxValidationResult::MempoolPolicy, "min relay fee not met"));
}

/// A chain of `n` spends from `from`, each paying 1,000 less.
fn chain_from(fx: &mut Fx, from: OutPoint, value: i64, n: usize) -> Vec<TxRef> {
    let mut txs = Vec::new();
    let (mut prev, mut value) = (from, value);
    for _ in 0..n {
        value -= 1_000;
        let tx = pay(&[prev], &[value]);
        fx.accept(&tx);
        prev = out(&tx, 0);
        txs.push(tx);
    }
    txs
}

/// Row 24 (`v:1078-1106`): over the ancestor limit is `too-long-mempool-chain`
/// (`TX_MEMPOOL_POLICY`) with `CalculateMemPoolAncestors`' message. The carve-out admits a
/// transaction of at most 10,000 vbytes with one in-pool ancestor whose descendants are at the
/// limit; a second such child, or an oversize one, fails with the first call's message.
#[test]
fn row_24_ancestor_limits_and_carve_out() {
    let mut fx = Fx::new();
    let coin = fx.fund_with(1, 1_000_000, 100, &p2sh_true());
    let txs = chain_from(&mut fx, coin, 1_000_000, 25);
    let next = pay(&[out(&txs[24], 0)], &[970_000]);
    let (result, _) = fx.submit(&next, ARGS);
    assert_eq!(
        (result.validation_result, result.reason.as_str(), result.debug_message.as_str()),
        (
            TxValidationResult::MempoolPolicy,
            "too-long-mempool-chain",
            "too many unconfirmed ancestors [limit: 25]"
        )
    );

    let mut fx = Fx::nonstandard();
    let coin = fx.fund_with(1, 1_000_000, 100, &p2sh_true());
    let parent = pay(&[coin], &[400_000, 100_000, 100_000, 100_000]);
    fx.accept(&parent);
    chain_from(&mut fx, out(&parent, 0), 400_000, 24);
    let descendants = format!("too many descendants for tx {} [limit: 25]", parent.txid());
    let script = p2sh_true();
    let big = vec![OP_TRUE; 10_001];
    let oversize = tx_of(
        2,
        TxType::Spend,
        &[(out(&parent, 1), &SIG_TRUE[..], u32::MAX)],
        &[(80_000, &script[..]), (0, &big[..])],
        0,
        vec![],
    );
    let (result, _) = fx.submit(&oversize, ARGS);
    assert_eq!(
        (result.reason.as_str(), result.debug_message.as_str()),
        ("too-long-mempool-chain", descendants.as_str())
    );

    fx.accept(&pay(&[out(&parent, 2)], &[90_000]));
    let (result, _) = fx.submit(&pay(&[out(&parent, 3)], &[90_000]), ARGS);
    assert_eq!(
        (result.reason.as_str(), result.debug_message.as_str()),
        ("too-long-mempool-chain", descendants.as_str())
    );
}

/// Row 25 (`v:1108-1114`): `CheckSpecialTxForMempool` at the tip with the tip's rules runs after
/// the ancestor limits, its reject unchanged.
#[test]
fn row_25_special_checks_after_ancestor_limits() {
    let mut fx = Fx::new();
    let coin = fx.fund(1);
    let tx = pay(&[coin], &[90_000]);
    fx.chain.special.insert(
        tx.txid(),
        TxValidationError::new(TxValidationResult::BadSpecial, "bad-special-test"),
    );
    assert_eq!(fx.verdict(&tx), no(TxValidationResult::BadSpecial, "bad-special-test"));
    assert_eq!(fx.chain.special_calls, vec![(tx.txid(), TIP)]);

    // Two in-pool ancestors are over a limit of two, with or without the carve-out.
    let mut fx = Fx::with_options(MemPoolOptions {
        limits: crate::options::MemPoolLimits {
            ancestor_count: 2,
            ..Default::default()
        },
        ..MemPoolOptions::default()
    });
    let coin = fx.fund(1);
    let txs = chain_from(&mut fx, coin, 100_000, 2);
    let child = pay(&[out(&txs[1], 0)], &[90_000]);
    fx.chain.special.insert(
        child.txid(),
        TxValidationError::new(TxValidationResult::BadSpecial, "bad-special-test"),
    );
    fx.chain.special_calls.clear();
    assert_eq!(fx.verdict(&child), no(TxValidationResult::MempoolPolicy, "too-long-mempool-chain"));
    assert_eq!(fx.chain.special_calls, vec![]);
}

/// Rows 26-27 (`v:1116-1135`): a provider-map conflict is `protx-dup` (`TX_CONFLICT`); so, after
/// it, is an operator key another held transaction claims under the other BLS scheme
/// (`existsProviderTxCrossSchemeConflict`, ungated; MPIX-13).
#[test]
fn rows_26_27_provider_conflicts() {
    let (p, q) = (TxHash::from([0x11; 32]), TxHash::from([0x22; 32]));
    let key = |n| operator_key(n, BlsSchemeKind::Basic);
    let mut fx = Fx::nonstandard();
    fx.chain.list = Arc::new(list_of(&[(1, q, key(1))]));
    let unknown = registrar(ProTxVersion::BasicBls, p, key(5), fx.fund(1));
    assert_eq!(fx.verdict(&unknown), no(TxValidationResult::Conflict, "protx-dup"));
    fx.chain.list = Arc::new(list_of(&[(1, q, key(1)), (2, p, key(2))]));
    assert_eq!(fx.verdict_with(&unknown, TEST_ACCEPT), ok());

    let held = registrar(ProTxVersion::BasicBls, q, key(5), fx.fund(2));
    let legacy =
        registrar(ProTxVersion::LegacyBls, p, operator_key(5, BlsSchemeKind::Legacy), fx.fund(3));
    assert_eq!(fx.verdict_with(&legacy, TEST_ACCEPT), ok());
    fx.accept(&held);
    {
        let state = fx.pool.pool.read();
        assert!(!state.exists_provider_tx_conflict(&legacy, &fx.chain.list));
        assert!(state.exists_provider_tx_cross_scheme_conflict(&legacy, &fx.chain.list));
    }
    assert_eq!(fx.verdict(&legacy), no(TxValidationResult::Conflict, "protx-dup"));
}

/// The script checks (`v:1186-1235`) run only after `PreChecks` passes: a script failure is
/// `PolicyScriptChecks`' verdict, while the same transaction under the relay fee fails row 22
/// first; both list their inputs for uncaching.
#[test]
fn script_checks_follow_prechecks() {
    let mut fx = Fx::nonstandard();
    let nop = fx.fund_with(1, 100_000, 100, &[OP_NOP1, OP_TRUE]);
    let script = p2sh_true();
    let spend = |value| {
        tx_of(2, TxType::Spend, &[(nop, &[], u32::MAX)], &[(value, &script[..])], 0, vec![])
    };
    let (result, uncache) = fx.submit(&spend(90_000), ARGS);
    assert_eq!(
        (result.validation_result, result.reason.as_str()),
        (
            TxValidationResult::NotStandard,
            "non-mandatory-script-verify-flag (NOPx reserved for soft-fork upgrades)"
        )
    );
    assert_eq!(uncache, vec![nop]);
    assert_eq!(
        fx.verdict(&spend(100_000)),
        no(TxValidationResult::MempoolPolicy, "min relay fee not met")
    );
}

/// A stateless record stage A computed for another instance is a wiring error, never a verdict.
#[test]
fn a_record_of_another_instance_is_fatal() {
    let mut fx = Fx::new();
    let tx = pay(&[fx.fund(1)], &[90_000]);
    let other = pay(&[fx.fund(2)], &[90_000]);
    let record = StatelessRecord::new(&other, &MemPoolOptions::default());
    let got = accept_to_memory_pool(&fx.pool, &mut fx.chain, &tx, &record, ARGS, Box::new(|_| {}));
    assert!(got.is_err());
    assert!(!fx.held(&tx));
}

/// `IsCurrentForFeeEstimation` (`v:412-423`): out of IBD, a tip no more than three hours old, at
/// most one block behind the best header.
#[test]
fn current_for_fee_estimation() {
    let now = 100_000;
    assert!(is_current_for_fee_estimation(false, now - 3 * 3600, now, 10, 11));
    assert!(!is_current_for_fee_estimation(true, now, now, 10, 10));
    assert!(!is_current_for_fee_estimation(false, now - 3 * 3600 - 1, now, 10, 10));
    assert!(!is_current_for_fee_estimation(false, now, now, 10, 12));
}

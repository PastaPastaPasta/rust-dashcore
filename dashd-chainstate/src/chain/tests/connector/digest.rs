//! CH3a a7-1: the StateDigest log of [`CoreConnector`]'s attempts through V (CH3 §2.15; O8 §4,
//! §5, §8), and [`ReplayChainHelper`]'s replays of a recording (CH3 §3.3; O8 §7.2), on the
//! connector harness.
//!
//! The expected values are computed apart from the sink: `undo` from CS5a's encoding of the coins
//! the block spent, the golden keys from a `coins::stats` run of the test's own from genesis,
//! `bur` from the deployment heights and `dep` from a fresh versionbits cache over V's chain.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use dash_chain::BlockRef;
use dash_consensus::coins::BlockUndo;
use dash_consensus::coins::MuHash3072;
use dash_consensus::coins::stats::{CoinStatsAccumulator, CoinStatsTotals, block_stats};
use dash_consensus::ctx::DeploymentPos;
use dash_consensus::subsidy::{SubsidyParams, block_subsidy_inner, genesis_reward};
use dash_statedigest::decisions::{Policy, RecordedDecisions};
use dash_statedigest::{
    Attempt, BestChainLock, Chain, Fields, FileKind, Hash256 as DHash, Header, Ident,
    IsConflictPair, Kind, Lines, Origin, Outcome, Query, Record, Value, Writer,
};

use super::*;
use crate::chain::deployments::ChainDeployments;
use crate::chain::digest_tap::{class_name, display};
use crate::chain::tests::support::{branch, with_tip_cx};
use crate::chain::{NotTested, ShutdownFlag as _, VerifyDbArgs, VerifyDbResult};
use crate::connect::{
    AttemptChoice, BlockAt, ConnectAttempt, DecisionTap, DigestAttempt, DigestLog, MissSlot,
    MuHashAt, RecordingHelper, ReplayChainHelper, RewindObserver, Rewound, TapedLog,
};

/// A digest log the test reads after the sink moves into the connector. With `lines` set, every
/// write after that many lines fails, or with `once` only the first such write.
#[derive(Clone, Default)]
struct Log {
    bytes: Arc<Mutex<Vec<u8>>>,
    lines: Option<usize>,
    once: bool,
    /// A write has failed.
    failed: Arc<AtomicBool>,
}

impl Write for Log {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes = self.bytes.lock().expect("log");
        let full = self.lines.is_some_and(|n| bytes.iter().filter(|&&b| b == b'\n').count() >= n);
        if full && !(self.once && self.failed.load(Ordering::SeqCst)) {
            self.failed.store(true, Ordering::SeqCst);
            return Err(io::Error::other("injected digest write failure"));
        }
        bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Log {
    fn bytes(&self) -> Vec<u8> {
        self.bytes.lock().expect("log").clone()
    }

    /// The records after the `H`, which must be [`header`].
    fn outcomes(&self) -> Vec<Outcome> {
        let bytes = self.bytes();
        let mut records = Lines::new(&bytes).map(|(_, r)| r.expect("a record"));
        assert_eq!(records.next(), Some(Record::Header(header(FileKind::Digest))));
        records
            .map(|r| match r {
                Record::Outcome(o) => o,
                other => panic!("{other:?} in a digest"),
            })
            .collect()
    }
}

fn header(file: FileKind) -> Header {
    Header {
        file,
        src: Writer::Rdashd,
        ver: b"test".to_vec(),
        chain: Chain::Regtest,
        run: b"1-1".to_vec(),
    }
}

/// `setup` with a connector that logs to `log`, stopping as the node does (CH3a D137).
fn logging_to(log: &Log, mut setup: Setup) -> Setup {
    let sink = DigestLog::new(log.clone(), header(FileKind::Digest)).expect("the H is written");
    setup.digest = Some(Box::new(sink));
    setup.node_stop = true;
    setup
}

/// A world whose connector logs to `log` ([`logging_to`]).
fn logged_to(log: Log, setup: Setup) -> (World, Log) {
    (world(logging_to(&log, setup)), log)
}

/// [`logged_to`] a new log.
fn logged(setup: Setup) -> (World, Log) {
    logged_to(Log::default(), setup)
}

fn hash(m: &Mined) -> DHash {
    display(m.hash()).expect("a block hash")
}

/// A record's kind, height, block and origin.
type Shape = (Kind, i64, DHash, Origin);

fn shapes(outcomes: &[Outcome]) -> Vec<Shape> {
    outcomes.iter().map(|o| (o.kind(), o.id.h, o.id.b, o.id.org)).collect()
}

fn c(m: &Mined) -> Shape {
    (Kind::Connect, m.height.into(), hash(m), Origin::Tip)
}

fn r(m: &Mined, org: Origin) -> Shape {
    (Kind::Reject, m.height.into(), hash(m), org)
}

fn d(m: &Mined) -> Shape {
    (Kind::Disconnect, m.height.into(), hash(m), Origin::Tip)
}

fn seqs(outcomes: &[Outcome]) -> Vec<i64> {
    outcomes.iter().map(|o| o.x().get("seq").and_then(Value::as_int).expect("a seq")).collect()
}

fn keys(fields: &Fields) -> Vec<&'static str> {
    fields.iter().map(|(spec, _)| spec.key).collect()
}

/// A `C`'s `s` keys: O8 §5.1–§5.3 in `FIELDS` order, without the evo keys CH7a's H11 writes.
const C_KEYS: [&str; 18] = [
    "prev",
    "ver",
    "mrkl",
    "time",
    "bits",
    "nonce",
    "work",
    "ntx",
    "undo",
    "bls",
    "bur",
    "dep",
    "muhash",
    "txouts",
    "amount",
    "unspendable",
    "bi",
    "stats",
];

/// A seeded, non-coinbase coin of `value` at height 1 (an undo coin of height 0 is the legacy
/// format, `v:2259-2266`).
fn seed_at1(n: u8, value: Amount) -> (OutPoint, TxOut, u32, bool) {
    (seed_op(n), out(value, &[OP_TRUE]), 1, false)
}

/// The undo of a block whose one transaction spent seed coin `value` (see [`seed_at1`]).
fn spent_seed(value: Amount) -> BlockUndo {
    BlockUndo {
        txs: vec![TxUndo {
            coins: vec![Coin::new(value, &[OP_TRUE], 1, false)],
        }],
    }
}

/// `Consensus::BuriedDeployment` in Core's enum order (`consensus/params.h:22-39`).
const BURIED: [BuriedDeployment; 15] = [
    BuriedDeployment::HeightInCoinbase,
    BuriedDeployment::DerSig,
    BuriedDeployment::Cltv,
    BuriedDeployment::Bip147,
    BuriedDeployment::Csv,
    BuriedDeployment::Dip0001,
    BuriedDeployment::Dip0003,
    BuriedDeployment::Dip0008,
    BuriedDeployment::Dip0020,
    BuriedDeployment::Dip0024,
    BuriedDeployment::Brr,
    BuriedDeployment::V19,
    BuriedDeployment::V20,
    BuriedDeployment::MnRr,
    BuriedDeployment::Withdrawals,
];

/// `dep` after the block `id` at `height`, from versionbits answers over V's chain.
fn derived_after(p: &Arc<ChainSetup>, exit: &VExit, id: BlockId, height: u32) -> [Value; 3] {
    let on = |d| p.consensus.deployment_height(d).to_u32() <= height + 1;
    let bur = BURIED.iter().enumerate().filter(|(_, d)| on(**d)).map(|(i, _)| 1 << i).sum();
    // A chain's deployments without the connector's rows read no EHF row.
    const NO_ROWS: &str = "no EHF rows, no read error";
    let deployments = ChainDeployments::new(p.clone(), &exit.chain);
    let view = deployments.view();
    let rows = [
        (DeploymentPos::TestDummy, "testdummy"),
        (DeploymentPos::V24, "v24"),
        (DeploymentPos::EvoShares, "evo_shares"),
    ]
    .map(|(pos, name)| {
        Value::Arr(vec![
            Value::string(name),
            Value::string(view.state(Some(id), pos).expect(NO_ROWS).name()),
            Value::Int(view.state_since_height(Some(id), pos).expect(NO_ROWS).into()),
        ])
    });
    [Value::Bool(!on(BuriedDeployment::V19)), Value::Int(bur), Value::Arr(rows.to_vec())]
}

/// The golden keys after the last of `chain` (genesis first, each block with what it spent): a
/// coinstats run from before genesis (`index/coinstatsindex.cpp:117-234`), with `stats` from
/// `getblockstats` (`null` at genesis).
fn golden(p: &ChainSetup, chain: &[(&Mined, BlockUndo)]) -> [(&'static str, Value); 6] {
    let reward =
        genesis_reward(&dash_consensus::params::genesis_block(p)).expect("a genesis reward");
    let sp = SubsidyParams::from_chain_params(p, reward).expect("subsidy parameters");
    let v20 = p.consensus.deployment_height(BuriedDeployment::V20).to_u32();
    let mut acc = CoinStatsAccumulator::new();
    let (mut prev, mut stats) = (None, Value::Null);
    for (i, (m, undo)) in chain.iter().enumerate() {
        let subsidy = match i.checked_sub(1).map(|k| chain[k].0) {
            None => reward,
            Some(parent) => {
                let bits = parent.block.header().bits();
                let height = i32::try_from(parent.height).expect("small");
                block_subsidy_inner(bits, height, m.height >= v20, &sp).expect("a subsidy")
            }
        };
        prev = (i > 0).then(|| *acc.totals());
        if i > 0 {
            let s = block_stats(&m.block, undo, subsidy).expect("stats");
            let row =
                [s.subsidy, s.totalfee, s.txs, s.ins, s.outs, s.utxo_increase, s.utxo_size_inc];
            stats = Value::Arr(row.iter().map(|&v| Value::Int(v)).collect());
        }
        acc.append_block(m.height, &m.block, undo, subsidy).expect("appended");
    }
    let muhash = DHash::from_wire_bytes(acc.finalize());
    let t = acc.totals();
    let b = t.block_info(prev.as_ref());
    let bi = [
        b.prevout_spent,
        b.coinbase,
        b.new_outputs_ex_coinbase,
        b.unspendable,
        b.unspendables_genesis_block,
        b.unspendables_scripts,
        b.unspendables_unclaimed_rewards,
    ];
    [
        ("muhash", Value::hash(&muhash)),
        ("txouts", Value::Int(t.transaction_output_count.try_into().expect("small"))),
        ("amount", Value::Int(t.total_amount)),
        ("unspendable", Value::Int(t.total_unspendable_amount)),
        ("bi", Value::Arr(bi.iter().map(|&v| Value::Int(v)).collect())),
        ("stats", stats),
    ]
}

/// `c`'s golden keys are [`golden`]'s over `chain`.
fn assert_golden(p: &ChainSetup, c: &Outcome, chain: &[(&Mined, BlockUndo)]) {
    for (key, want) in golden(p, chain) {
        assert_eq!(c.s().get(key), Some(&want), "{key}");
    }
}

/// `c`'s derived keys but `undo` are [`derived_after`]'s for `m`.
fn assert_derived(p: &Arc<ChainSetup>, exit: &VExit, c: &Outcome, m: &Mined, id: BlockId) {
    let [bls, bur, dep] = derived_after(p, exit, id, m.height);
    assert_eq!(c.s().get("bls"), Some(&bls));
    assert_eq!(c.s().get("bur"), Some(&bur));
    assert_eq!(c.s().get("dep"), Some(&dep));
}

/// O8 §5: a connected block's `C` holds the header keys, the derived keys and the golden keys,
/// in `FIELDS` order, the evo keys and `fx` absent (CH3a D135), and its `seq`. `undo` is the
/// `sha256d` of CS5a's encoding of the coins the block spent.
#[test]
fn a_connect_records_every_key_of_its_c() {
    let (mut w, log) = logged(Setup {
        seeds: vec![seed_at1(1, 50 * COIN)],
        ..Setup::default()
    });
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![spend(&[seed_op(1)], 49 * COIN)]);
    w.rig.accept(&b1);
    let id = w.rig.id(&b1);
    let exit = w.rig.stop();
    let outcomes = log.outcomes();
    assert_eq!(shapes(&outcomes), [c(&g), c(&b1)]);
    let c1 = &outcomes[1];
    assert_eq!(keys(c1.s()), C_KEYS);
    assert!(c1.o().is_empty(), "fx is absent");
    assert_eq!(keys(c1.x()), ["seq"]);
    assert_eq!(c1.x().get("seq"), Some(&Value::Int(2)));

    let s = c1.s();
    let header = b1.block.header();
    assert_eq!(s.get("prev"), Some(&Value::hash(&hash(&g))));
    assert_eq!(s.get("mrkl"), Some(&Value::hash(&display(header.merkle_root()).expect("hex"))));
    assert_eq!(s.get("ntx"), Some(&Value::Int(2)));
    let undo = spent_seed(50 * COIN);
    let want = DHash::from_wire_bytes(dash_sha256::sha256d(undo.core_bytes().as_slice()));
    assert_eq!(s.get("undo"), Some(&Value::hash(&want)));
    assert_derived(&p, &exit, c1, &b1, id);
    assert_golden(&p, c1, &[(&g, BlockUndo::default()), (&b1, undo)]);
    // The block paid itself 1 of its subsidy and 1 of fees: the rest is unclaimed.
    let stats = s.get("stats").and_then(Value::as_arr).expect("stats");
    assert_eq!(stats[1], Value::Int(COIN), "totalfee");
}

/// O8 §5 at genesis: `ConnectBlock` returns before it streams an undo (`v:2534-2540`), so `undo`
/// and `stats` are `null`; the coinstats index counts genesis's subsidy as unspendable
/// (`index/coinstatsindex.cpp:124-128`), so the set is empty.
#[test]
fn genesis_records_a_c_without_undo_or_stats() {
    let (mut w, log) = logged(Setup::default());
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let id = w.rig.id(&g);
    let exit = w.rig.stop();
    let outcomes = log.outcomes();
    assert_eq!(shapes(&outcomes), [c(&g)]);
    let s = outcomes[0].s();
    assert_eq!(keys(s), C_KEYS);
    for key in ["prev", "undo", "stats"] {
        assert_eq!(s.get(key), Some(&Value::Null), "{key}");
    }
    let reward = genesis_reward(&dash_consensus::params::genesis_block(&p)).expect("a reward");
    assert_eq!(s.get("txouts"), Some(&Value::Int(0)));
    assert_eq!(s.get("amount"), Some(&Value::Int(0)));
    assert_eq!(s.get("unspendable"), Some(&Value::Int(reward)));
    assert_derived(&p, &exit, &outcomes[0], &g, id);
    assert_golden(&p, &outcomes[0], &[(&g, BlockUndo::default())]);
}

/// V's deployments without their versionbits answers ([`DeploymentView::versionbits`]'s
/// default).
struct NoVersionbits<'a>(&'a dyn DeploymentView);

impl DeploymentView for NoVersionbits<'_> {
    fn active_after(&self, prev: Option<BlockId>, d: Deployment) -> bool {
        self.0.active_after(prev, d)
    }

    fn active_at(&self, id: BlockId, d: Deployment) -> bool {
        self.0.active_at(id, d)
    }
}

/// O8 §5.2: `dep` is the versionbits answers for the block's child, which every `C` holds. Without
/// them the record fails, as without its `undo` or subsidy, rather than leave `dep` absent.
#[test]
fn a_c_without_versionbits_answers_fails_its_record() {
    let mut w = world(Setup::default());
    let g = w.rig.genesis.clone();
    let b1 = block_on(&w.rig.p, &g, vec![]);
    w.rig.accept(&b1);
    let at = BlockAt {
        hash: b1.hash(),
        height: 1,
        prev: Some(w.rig.id(&g)),
        id: Some(w.rig.id(&b1)),
    };
    let log = Log::default();
    let mut sink = DigestLog::new(log.clone(), header(FileKind::Digest)).expect("the H");
    let block = b1.block.clone();
    let recorded = super::disconnect::on_v_cx(&w, AttemptOrigin::Tip, move |cx, _, _| {
        let cx = ConnectCx {
            deployments: &NoVersionbits(cx.deployments),
            ..*cx
        };
        let attempt = ConnectAttempt {
            block: &block,
            at: &at,
            just_check: false,
            active: false,
            outcome: &Ok(ConnectOutcome::Connected),
            undo: Some(&[]),
            subsidy: None,
            script_checks: Some(true),
            evo: None,
        };
        sink.record(&cx, &DigestAttempt::Connect(attempt))
    });
    let want = "statedigest: no versionbits answers for the block at 1";
    assert_eq!(recorded, Err(Fatal::new(want)));
    assert!(log.outcomes().is_empty());
    w.rig.stop();
}

/// `GetBlockSubsidy(pindex)` (`v:1884-1889`) takes V20 as `DeploymentActiveAt(*pindex)`, so the
/// block before V20 has the pre-V20 subsidy (`GetBlockSubsidyHelper`, `v:1809-1870`: the
/// difficulty-era base, and above `nBudgetPaymentsStartBlock` a treasury part of 1/10, where V20's
/// are a base of 5 and 1/5). Regtest with V20 at 6 (`-dip3params=6:6`,
/// `-testactivationheight=v20@6`, `mn_rr@6`, as Core requires them no lower) and the budget from 1
/// (`-budgetparams=240:1:1500`): G1 refuses block 6, whose parent has DIP0003 active after it, but
/// not block 5, whose `stats[0]` and `bi[6]` are the pre-V20 subsidy's.
#[test]
fn the_c_before_v20_has_the_pre_v20_subsidy() {
    const V20: u32 = 6;
    let p = Arc::new(
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .arg(ChainOption::Dip3Params, format!("{V20}:{V20}"))
            .arg(ChainOption::TestActivationHeight, format!("v20@{V20}"))
            .arg(ChainOption::TestActivationHeight, format!("mn_rr@{V20}"))
            .arg(ChainOption::BudgetParams, "240:1:1500")
            .build()
            .expect("regtest"),
    );
    assert_eq!(p.consensus.deployment_height(BuriedDeployment::V20).to_u32(), V20);
    let (mut w, log) = logged(Setup {
        params: Some(p.clone()),
        ..Setup::default()
    });
    let mut chain = vec![w.rig.genesis.clone()];
    for _ in 1..V20 {
        let next = block_on(&p, chain.last().expect("genesis"), vec![]);
        w.rig.accept(&next);
        chain.push(next);
    }
    w.rig.stop();
    let outcomes = log.outcomes();
    assert_eq!(shapes(&outcomes), chain.iter().map(c).collect::<Vec<_>>());
    let (last, parent) = (&outcomes[5], &chain[4]);
    assert_eq!(chain[5].height, V20 - 1);

    let reward =
        genesis_reward(&dash_consensus::params::genesis_block(&p)).expect("a genesis reward");
    let sp = SubsidyParams::from_chain_params(&p, reward).expect("subsidy parameters");
    let bits = parent.block.header().bits();
    let at = |v20| block_subsidy_inner(bits, 4, v20, &sp).expect("a subsidy");
    let subsidy = at(false);
    assert_ne!(subsidy, at(true), "V20 changes it");
    let stats = last.s().get("stats").and_then(Value::as_arr).expect("stats");
    assert_eq!(stats[0], Value::Int(subsidy), "stats[0], the subsidy");
    let bi = last.s().get("bi").and_then(Value::as_arr).expect("bi");
    assert_eq!(bi[6], Value::Int(subsidy - COIN), "bi[6], the unclaimed rewards");
    let undone: Vec<_> = chain.iter().map(|m| (m, BlockUndo::default())).collect();
    assert_golden(&p, last, &undone);
}

/// An `R`'s `dbg`: the debug message, `null` when empty (O8's patch, `statedigest.cpp:209`).
fn dbg(err: &BlockValidationError) -> Value {
    match err.debug_message() {
        "" => Value::Null,
        message => Value::string(message),
    }
}

/// O8 §4.3: an invalid block's `R` holds Core's class and reason, its debug message as `dbg`.
#[test]
fn an_invalid_block_records_its_r() {
    let (mut w, log) = logged(Setup::default());
    let g = w.rig.genesis.clone();
    let b1 = block_on(&w.rig.p, &g, vec![spend(&[seed_op(9)], COIN)]);
    let err = last_invalid(&w, std::slice::from_ref(&b1));
    w.rig.stop();
    let outcomes = log.outcomes();
    assert_eq!(shapes(&outcomes), [c(&g), r(&b1, Origin::Tip)]);
    let rec = &outcomes[1];
    assert_eq!(keys(rec.s()), ["res", "cls", "why"]);
    assert_eq!(rec.s().get("res"), Some(&Value::string("invalid")));
    assert_eq!(rec.s().get("cls"), Some(&Value::string(class_name(err.result()))));
    assert_eq!(rec.s().get("cls"), Some(&Value::string("consensus")));
    assert_eq!(rec.s().get("why"), Some(&Value::string("bad-txns-inputs-missingorspent")));
    assert!(!err.debug_message().is_empty());
    assert_eq!(rec.x().get("dbg"), Some(&dbg(&err)));
    assert_eq!(seqs(&outcomes), [1, 2]);
    assert!(rec.o().is_empty(), "fx is absent");
}

/// O8 §4.4 and §5.3 over a reorg and back: each tip disconnect is a `D` `ok`, and rewinds the
/// golden state as Core's coinstats index does (`index/coinstatsindex.cpp:264-299`): the side
/// branch's golden keys are a run over genesis and that branch, and `b1`'s reconnect `C` is its
/// first one. One `seq` numbers the `C`s and `D`s.
#[test]
fn a_reorg_records_its_ds_and_rewinds_the_golden_keys() {
    let (mut w, log) = logged(Setup {
        seeds: vec![seed_at1(1, 50 * COIN)],
        ..Setup::default()
    });
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![spend(&[seed_op(1)], 49 * COIN)]);
    let a = branch(&p, &b1, 3, 0);
    let side = branch(&p, &g, 3, 1);
    w.rig.accept(&b1);
    w.rig.accept(&a[0]);
    w.rig.headers(&side);
    for m in &side {
        w.rig.accept(m);
    }
    w.rig.accept(&a[1]);
    w.rig.accept(&a[2]);
    w.rig.stop();

    let outcomes = log.outcomes();
    assert_eq!(
        shapes(&outcomes),
        [
            c(&g),
            c(&b1),
            c(&a[0]),
            d(&a[0]),
            d(&b1),
            c(&side[0]),
            c(&side[1]),
            c(&side[2]),
            d(&side[2]),
            d(&side[1]),
            d(&side[0]),
            c(&b1),
            c(&a[0]),
            c(&a[1]),
            c(&a[2]),
        ]
    );
    assert_eq!(seqs(&outcomes), (1..=15).collect::<Vec<_>>());
    for o in outcomes.iter().filter(|o| o.kind() == Kind::Disconnect) {
        assert_eq!(keys(o.s()), ["res"]);
        assert_eq!(o.s().get("res"), Some(&Value::string("ok")));
        assert!(o.o().is_empty(), "fx is absent");
    }
    let empty = BlockUndo::default;
    assert_golden(&p, &outcomes[5], &[(&g, empty()), (&side[0], empty())]);
    let side_chain = [(&g, empty()), (&side[0], empty()), (&side[1], empty()), (&side[2], empty())];
    assert_golden(&p, &outcomes[7], &side_chain);
    // The rewinds of the side branch leave genesis's state again: `b1` and `a2` record what
    // they recorded first, their undo hashed again over a stored undo (`HAVE_UNDO`).
    assert_eq!(outcomes[11].s(), outcomes[1].s());
    assert_eq!(outcomes[12].s(), outcomes[2].s());
    let main = [(&g, empty()), (&b1, spent_seed(50 * COIN)), (&a[0], empty()), (&a[1], empty())];
    assert_golden(&p, &outcomes[13], &main);
}

/// O8 §4.4 (CH3a D133, D141): an unclean and a failed disconnect are `D`s `unclean` and `failed`,
/// as O8's patch writes them (`AttemptScope::Disconnected`), and neither rewinds the golden state:
/// the coinstats index rewinds only the active chain's blocks (`index/coinstatsindex.cpp:264-299`),
/// and a failed `DisconnectTip` leaves its block the tip. So the next active `C` continues from the
/// block, past D141's check, with the keys of a run that never left it. The cases: `VerifyDB`'s
/// unclean disconnect into its own view (`v:5113-5122`); `DisconnectTip`'s unclean one and its
/// failed undo read (`v:3281-3284`, `UNDO_READ_FAILED`), which only end `InvalidateBlock`'s walk
/// (`v:3966`); and a `Fatal`, an exception out of `DisconnectBlock`, of a tip disconnect called on
/// the connector, so that V goes on.
#[test]
fn an_unclean_or_failed_disconnect_records_its_res_and_rewinds_nothing() {
    use super::disconnect::{
        Scratch, disconnect_on_v, edit, invalidate, record_len, spent, undo_record, write_at,
    };
    let cases = [
        ("verifydb unclean", Origin::Verifydb, "unclean"),
        ("tip unclean", Origin::Tip, "unclean"),
        ("tip failed", Origin::Tip, "failed"),
        ("tip fatal", Origin::Tip, "failed"),
    ];
    for (n, (case, org, res)) in (40..).zip(cases) {
        let log = Log::default();
        let (mut w, b1, t1) = spent(n, logging_to(&log, Setup::default()));
        let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
        let made = op(&t1, 0);
        match case {
            "verifydb unclean" => {
                // The coinbase's output is missing from the view.
                let best = BlockRef {
                    id: w.rig.id(&b1),
                    hash: b1.hash(),
                    height: 1,
                };
                let held = [(made, t1.outputs()[0].clone(), 1, false)];
                let (result, _) = disconnect_on_v(&w, Some(Scratch::at(best, &held)), &b1);
                assert_eq!(result, Ok(DisconnectResult::Unclean));
            }
            "tip unclean" => {
                edit(&w, |ov| {
                    assert!(ov.spend(&OutPointKey::new(&made)).expect("reads").is_some());
                });
                invalidate(&w, &b1);
            }
            "tip failed" => {
                let (path, at) = undo_record(&w, &b1);
                write_at(&path, at + record_len(&path, at), &[0; 32]);
                invalidate(&w, &b1);
            }
            _ => {
                w.seen.lock().expect("seen").fail_next_undo = true;
                let (result, _) = disconnect_on_v(&w, None, &b1);
                assert_eq!(result, Err(Fatal::new(UNDO_FATAL)));
            }
        }
        assert_eq!(w.rig.v.tip_watch().get(), Some(b1.hash()), "{case}");
        let b2 = block_on(&p, &b1, vec![]);
        w.rig.accept(&b2);
        let exit = w.rig.stop();
        assert_eq!(fatal_text(&exit), None, "{case}");
        w.rig.assert_no_abort();
        let outcomes = log.outcomes();
        let d1 = (Kind::Disconnect, 1, hash(&b1), org);
        assert_eq!(shapes(&outcomes), [c(&g), c(&b1), d1, c(&b2)], "{case}");
        assert_eq!(keys(outcomes[2].s()), ["res"], "{case}");
        assert_eq!(outcomes[2].s().get("res"), Some(&Value::string(res)), "{case}");
        let empty = BlockUndo::default;
        let chain = [(&g, empty()), (&b1, spent_seed(50 * COIN)), (&b2, empty())];
        assert_golden(&p, &outcomes[3], &chain);
    }
}

/// BL-775 (CH7 D102): a disconnect whose undo snapshot write the latch refused, after another
/// route's write failed first and their store reported it, is `DisconnectBlock`'s `failed`, as
/// O8's patch writes Core's `DISCONNECT_FAILED` from the undo's catch: the tip's and `VerifyDB`'s
/// alike, and the active coins commit nothing.
#[test]
fn a_disconnect_whose_undo_write_was_refused_records_failed() {
    use super::disconnect::{Scratch, arm_undo_store_fault, disconnect_on_v, spent};
    for (n, scratch) in [(162, false), (163, true)] {
        let log = Log::default();
        let (w, b1, t1) = spent(n, logging_to(&log, Setup::default()));
        let g = w.rig.genesis.clone();
        arm_undo_store_fault(&w.seen, &w.sink, g.hash());
        w.seen.lock().expect("seen").undo_after_a_report = true;
        let (view, org) = if scratch {
            let best = BlockRef {
                id: w.rig.id(&b1),
                hash: b1.hash(),
                height: 1,
            };
            let held = [(op(&t1, 0), t1.outputs()[0].clone(), 1, false)];
            (Some(Scratch::at(best, &held)), Origin::Verifydb)
        } else {
            (None, Origin::Tip)
        };
        let (result, _) = disconnect_on_v(&w, view, &b1);
        assert_eq!(result, Ok(DisconnectResult::Failed), "scratch {scratch}");
        let outcomes = log.outcomes();
        let d1 = (Kind::Disconnect, 1, hash(&b1), org);
        assert_eq!(shapes(&outcomes), [c(&g), c(&b1), d1], "scratch {scratch}");
        assert_eq!(outcomes[2].s().get("res"), Some(&Value::string("failed")), "scratch {scratch}");
        assert_eq!(w.rig.v.tip_watch().get(), Some(b1.hash()), "scratch {scratch}");
        assert_eq!(coins_at(&w.coins), Some(b1.hash()), "scratch {scratch}");
    }
}

/// What a [`RewindObserver`] saw; with `fail_at` set, the rewind to that height is refused.
#[derive(Clone, Default)]
struct Seen {
    rewound: Arc<Mutex<Vec<Rewound>>>,
    fail_at: Option<u32>,
}

impl RewindObserver for Seen {
    fn rewound(&mut self, to: &Rewound) -> Result<(), Fatal> {
        self.rewound.lock().expect("seen").push(to.clone());
        match self.fail_at {
            Some(h) if h == to.height => Err(Fatal::new(format!("reverse leg: muhash at {h}"))),
            _ => Ok(()),
        }
    }
}

/// A world whose log's rewinds `seen` observes, stopping as the node does (CH3a D137).
fn observed(seen: &Seen) -> (World, Log) {
    observed_at(seen, MuHashAt::Every)
}

/// [`observed`], the log finalizing the MuHash at `at`'s blocks.
fn observed_at(seen: &Seen, at: MuHashAt) -> (World, Log) {
    let log = Log::default();
    let sink = DigestLog::new(log.clone(), header(FileKind::Digest))
        .expect("the H is written")
        .with_muhash_at(at)
        .with_observer(Box::new(seen.clone()));
    let w = world(Setup {
        seeds: vec![seed_at1(1, 50 * COIN)],
        digest: Some(Box::new(sink)),
        node_stop: true,
        ..Setup::default()
    });
    (w, log)
}

/// The golden keys a rewind to `c`'s block must give: its `muhash`, `txouts` and `amount`.
fn rewound_to(c: &Outcome) -> Rewound {
    let s = c.s();
    Rewound {
        height: c.id.h.try_into().expect("a height"),
        hash: c.id.b,
        muhash: match s.get("muhash") {
            Some(Value::Str(hex)) => Some(DHash::from_hex(hex).expect("a muhash")),
            other => panic!("muhash {other:?}"),
        },
        txouts: s.get("txouts").and_then(Value::as_int).expect("txouts").try_into().expect("u64"),
        amount: s.get("amount").and_then(Value::as_int).expect("amount"),
    }
}

/// The reorg of [`a_reorg_records_its_ds_and_rewinds_the_golden_keys`] in a world whose log's
/// rewinds `seen` observes, its MuHash finalized at `at`'s blocks: the log's records, whose
/// shapes `at` does not change.
fn observed_reorg(seen: &Seen, at: MuHashAt) -> Vec<Outcome> {
    let (mut w, log) = observed_at(seen, at);
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![spend(&[seed_op(1)], 49 * COIN)]);
    let a = branch(&p, &b1, 3, 0);
    let side = branch(&p, &g, 3, 1);
    w.rig.accept(&b1);
    w.rig.accept(&a[0]);
    w.rig.headers(&side);
    for m in &side {
        w.rig.accept(m);
    }
    w.rig.accept(&a[1]);
    w.rig.accept(&a[2]);
    w.rig.stop();
    let outcomes = log.outcomes();
    assert_eq!(outcomes.len(), 15, "the log of the reorg test");
    assert_eq!(shapes(&outcomes[3..5]), [d(&a[0]), d(&b1)]);
    assert_eq!(shapes(&outcomes[8..11]), [d(&side[2]), d(&side[1]), d(&side[0])]);
    outcomes
}

/// CH3a D152: after each clean rewind of the active coins the observer sees the accumulator at
/// the disconnected block's parent, whose golden keys are the parent's `C`'s: here the reorg of
/// [`a_reorg_records_its_ds_and_rewinds_the_golden_keys`], whose log is unchanged by it.
#[test]
fn a_rewind_observer_sees_each_parents_golden_keys() {
    let seen = Seen::default();
    let outcomes = observed_reorg(&seen, MuHashAt::Every);
    // a0 down to b1, b1 to genesis, then the side branch down to genesis.
    let want: Vec<Rewound> = [1, 0, 6, 5, 0].iter().map(|&i| rewound_to(&outcomes[i])).collect();
    assert_eq!(*seen.rewound.lock().expect("seen"), want);
    assert_ne!(want[0].muhash, want[1].muhash, "b1 spent and created coins");
}

/// CH3a D165: [`MuHashAt::Only`] finalizes at its heights alone. Its log is the full log with
/// `muhash` absent from the other `C`s, and its rewinds are the full log's with `muhash` absent
/// at the other parents, though the reorg visits height 1 and genesis on two branches.
#[test]
fn muhash_at_only_finalizes_at_its_heights() {
    let (all, only) = (Seen::default(), Seen::default());
    let full = observed_reorg(&all, MuHashAt::Every);
    let picked = observed_reorg(&only, MuHashAt::Only([1].into()));
    assert_eq!(full.len(), picked.len());
    let mut finalized = 0;
    for (f, o) in full.iter().zip(&picked) {
        let mut want = f.clone();
        if f.kind() == Kind::Connect && f.id.h != 1 {
            want.s_mut().remove("muhash");
        }
        finalized += usize::from(o.s().get("muhash").is_some());
        assert_eq!(*o, want, "{:?} at {}", o.kind(), o.id.h);
    }
    assert_eq!(finalized, 3, "b1, side[0] and b1 again");
    let want: Vec<Rewound> = all
        .rewound
        .lock()
        .expect("seen")
        .iter()
        .map(|r| Rewound {
            muhash: r.muhash.filter(|_| r.height == 1),
            ..r.clone()
        })
        .collect();
    assert_eq!(*only.rewound.lock().expect("seen"), want);
    let at1 = want.iter().filter(|r| r.muhash.is_some()).count();
    assert_eq!(at1, 2, "a0 down to b1, side[1] down to side[0]");
}

/// A sink that drops the first attempt, genesis's `C`, and hands the rest to its log.
struct AfterGenesis(DigestLog<Log>);

impl DigestSink for AfterGenesis {
    fn record(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        if matches!(attempt, DigestAttempt::Connect(c) if c.at.prev.is_none()) {
            return Ok(());
        }
        self.0.record(cx, attempt)
    }
}

/// CH3a D165: a log seeded at genesis as a pruned start seeds it writes from block 1 on every `s`
/// key the log kept from genesis writes. The seed is the set's MuHash state, its `txouts` and
/// `amount`, genesis's `unspendable`, and `total_subsidy` equal to it, the other totals 0: `bi` is
/// a difference of two totals, and the unclaimed rewards read only the index's balance
/// (`index/coinstatsindex.cpp:208`, `:482`), which is 0 after every block in Core and in the
/// seed. The set is the one the full log holds after genesis, empty: the world's planted coin is
/// outside both logs. With `total_subsidy` 0 the balance is off and `bi` differs.
#[test]
fn a_seeded_log_writes_the_keys_of_a_log_from_genesis() {
    let blocks = |w: &mut World| {
        let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
        let b1 = block_on(&p, &g, vec![spend(&[seed_op(1)], 49 * COIN)]);
        let b2 = block_on(&p, &b1, vec![]);
        w.rig.accept(&b1);
        w.rig.accept(&b2);
        w.rig.stop();
    };
    let setup = || Setup {
        seeds: vec![seed_at1(1, 50 * COIN)],
        ..Setup::default()
    };
    let (mut w, log) = logged(setup());
    blocks(&mut w);
    let full = log.outcomes();
    assert_eq!(full.len(), 3, "genesis, b1 and b2");
    let unspendable = full[0].s().get("unspendable").and_then(Value::as_int).expect("an int");
    assert!(unspendable > 0, "genesis's output is unspendable");

    let seeded = |total_subsidy| {
        let log = Log::default();
        let totals = CoinStatsTotals {
            total_unspendable_amount: unspendable,
            total_subsidy,
            ..CoinStatsTotals::default()
        };
        let stats = CoinStatsAccumulator::from_parts(MuHash3072::new(), totals);
        let sink = DigestLog::seeded(log.clone(), header(FileKind::Digest), stats, BlockId(0))
            .expect("the H is written");
        let mut w = world(Setup {
            digest: Some(Box::new(AfterGenesis(sink))),
            node_stop: true,
            ..setup()
        });
        assert_eq!(w.rig.id(&w.rig.genesis.clone()), BlockId(0), "the seed's block");
        blocks(&mut w);
        let after = log.outcomes();
        assert_eq!(after.len(), 2);
        after
    };
    for (f, s) in full[1..].iter().zip(&seeded(unspendable)) {
        assert_eq!((s.id.clone(), s.s()), (f.id.clone(), f.s()), "at {}", f.id.h);
    }
    let off = seeded(0);
    assert_ne!(off[0].s().get("bi"), full[1].s().get("bi"), "an unbalanced seed");
}

/// CH3a D152, D137: an observer's error fails the record, as a failed write does. It is an
/// `AbortNode` and the disconnect goes on: it flushes (`v:3285-3287`), and V stops on the latch
/// at `DisconnectTip`'s IF_NEEDED site (`v:3304`) before its tip moves, with the observer's text
/// reported once, the `D` written and nothing after it.
#[test]
fn an_observer_error_stops_v_at_that_rewind() {
    let seen = Seen {
        fail_at: Some(1),
        ..Seen::default()
    };
    let (mut w, log) = observed(&seen);
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![]);
    let b2 = block_on(&p, &b1, vec![]);
    let side = branch(&p, &b1, 2, 1);
    w.rig.accept(&b1);
    w.rig.accept(&b2);
    w.rig.headers(&side);
    w.rig.accept(&side[0]);
    w.rig.submit_stopping(&side[1], "the reorg's refused rewind");
    assert_eq!(coins_at(&w.coins), Some(b1.hash()), "the disconnect flushed");
    let exit = w.rig.stop();
    let text = "reverse leg: muhash at 1";
    assert_eq!(fatal_text(&exit), Some(text));
    assert_eq!(w.sink.messages(), [text], "one report");
    assert!(w.rig.shutdown.shutdown_requested(), "the node's shutdown");
    let at = exit.chain.private_tip().map(|(t, _)| t.hash);
    assert_eq!(at, Some(b2.hash()), "V stops before the tip moves");
    assert_eq!(shapes(&log.outcomes()), [c(&g), c(&b1), c(&b2), d(&b2)]);
    assert_eq!(seen.rewound.lock().expect("seen").len(), 1);
}

/// A connect attempt's height and `script_checks`.
type Decision = (u32, Option<bool>);

/// Each connect attempt's [`Decision`], as the seam hands them over.
#[derive(Clone, Default)]
struct Checks(Arc<Mutex<Vec<Decision>>>);

impl DigestSink for Checks {
    fn record(&mut self, _: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        if let DigestAttempt::Connect(c) = attempt {
            self.0.lock().expect("checks").push((c.at.height, c.script_checks));
        }
        Ok(())
    }
}

/// CH3a D151: `ConnectAttempt::script_checks` is `ConnectBlock`'s `fScriptChecks`
/// (`v:2542-2571`), `None` for genesis, which returns before deciding (`v:2536-2540`). With
/// `-assumevalid` at b3, the best header, b1 is more than two weeks of proof-equivalent time
/// behind it and skips (its failing script connects); b2 and b3 are within (condition 5,
/// `v:2568`) and check. Without `-assumevalid`, every block checks, and b1 is rejected.
#[test]
fn a_connect_attempt_carries_its_script_checks_decision() {
    let c = AvChain::new();
    let run = |av: Option<BlockHash>| {
        let checks = Checks::default();
        let mut w = world(Setup {
            params: Some(c.p.clone()),
            seeds: vec![seed(1, 50 * COIN, &[0x00])],
            assume_valid: av,
            digest: Some(Box::new(checks.clone())),
            ..Setup::default()
        });
        w.rig.headers(&c.b);
        for m in &c.b {
            w.rig.submit(m);
        }
        w.rig.stop();
        checks.0.lock().expect("checks").clone()
    };
    assert_eq!(
        run(Some(c.b[2].hash())),
        [(0, None), (1, Some(false)), (2, Some(true)), (3, Some(true))]
    );
    assert_eq!(
        run(None),
        [(0, None), (1, Some(true))],
        "b1 is rejected, so b2 and b3 never connect"
    );
}

/// A log that misses the `skip`-th attempt (from 1): every other one reaches `inner`.
struct Missing {
    inner: DigestLog<Log>,
    skip: usize,
    seen: usize,
}

impl DigestSink for Missing {
    fn record(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        self.seen += 1;
        if self.seen == self.skip {
            return Ok(());
        }
        self.inner.record(cx, attempt)
    }
}

fn missing(skip: usize, setup: Setup) -> (World, Log) {
    let log = Log::default();
    let inner = DigestLog::new(log.clone(), header(FileKind::Digest)).expect("the H");
    let sink = Missing {
        inner,
        skip,
        seen: 0,
    };
    let w = world(Setup {
        digest: Some(Box::new(sink)),
        node_stop: true,
        ..setup
    });
    (w, log)
}

/// CH3a D141: the golden keys need the accumulator at the block's parent, as the coinstats index
/// checks its last entry (`index/coinstatsindex.cpp:133-139`, `:417-422`). A log that missed a
/// block stops V rather than write keys from a gap: here genesis's `C`, then a block's `C` that
/// a reorg rewinds. As every failed record, it fails the recording only (D137): the attempt
/// commits, and V stops on the latch after it.
#[test]
fn a_log_that_missed_a_block_stops_v() {
    let (mut w, log) = missing(1, Setup::default());
    let g = w.rig.genesis.clone();
    let b1 = block_on(&w.rig.p, &g, vec![]);
    w.rig.submit_stopping(&b1, "block 1 after a missed genesis");
    let exit = w.rig.stop();
    assert_eq!(
        fatal_text(&exit),
        Some(
            "statedigest: the coin stats are not at the parent of the block at 1: the log missed \
             a block"
        )
    );
    assert_eq!(coins_at(&w.coins), Some(b1.hash()), "committed");
    assert!(log.outcomes().is_empty());

    // Genesis, b1, then b2's `C` is missed; the reorg to the side branch disconnects b2.
    let (mut w, log) = missing(3, Setup::default());
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![]);
    let b2 = block_on(&p, &b1, vec![]);
    let side = branch(&p, &b1, 2, 1);
    w.rig.accept(&b1);
    w.rig.accept(&b2);
    w.rig.headers(&side);
    w.rig.accept(&side[0]);
    w.rig.submit_stopping(&side[1], "the reorg's rewind of the missed block");
    let exit = w.rig.stop();
    assert_eq!(
        fatal_text(&exit),
        Some(
            "statedigest: the coin stats are not at the block at 2 the disconnect rewinds: the \
             log missed a block"
        )
    );
    assert_eq!(coins_at(&w.coins), Some(b1.hash()), "the disconnect is committed");
    assert_eq!(shapes(&log.outcomes()), [c(&g), c(&b1), d(&b2)]);
}

/// A log opened after `from` attempts, as one opened when a process restarts on a chain.
struct Late {
    inner: DigestLog<Log>,
    from: usize,
    seen: usize,
}

impl DigestSink for Late {
    fn record(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        self.seen += 1;
        if self.seen <= self.from {
            return Ok(());
        }
        self.inner.record(cx, attempt)
    }
}

/// CH3a a10 Q3: a log [`DigestLog::without_golden`] opens on a chain it did not see from genesis,
/// as O2b's records plane does after a restore. Its connect, its proposal and a reorg's
/// disconnects each write their record without D141's stop: no `C` has a golden key and no
/// disconnect rewinds. Every other key is what a log from genesis writes.
#[test]
fn a_log_without_golden_keys_opens_on_any_chain() {
    let log = Log::default();
    let inner = DigestLog::without_golden(log.clone(), header(FileKind::Digest)).expect("the H");
    let (full, full_log) = logged(Setup::default());
    let mut w = world(Setup {
        digest: Some(Box::new(Late {
            inner,
            from: 3,
            seen: 0,
        })),
        node_stop: true,
        ..Setup::default()
    });
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![]);
    let b2 = block_on(&p, &b1, vec![]);
    let b3 = block_on(&p, &b2, vec![]);
    let side = branch(&p, &b1, 3, 1);
    let mut full = full;
    for w in [&mut w, &mut full] {
        w.rig.accept(&b1);
        w.rig.accept(&b2);
        assert_eq!(propose(w, &b3, false, true), Ok(BlockState::Valid));
        w.rig.accept(&b3);
        w.rig.headers(&side);
        for m in &side {
            w.rig.accept(m);
        }
    }
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), None, "no D141 stop");
    full.rig.stop();
    assert_eq!(coins_at(&w.coins), Some(side[2].hash()), "the reorg ran");

    let outcomes = log.outcomes();
    assert_eq!(
        shapes(&outcomes),
        [r(&b3, Origin::Proposal), c(&b3), d(&b3), d(&b2), c(&side[0]), c(&side[1]), c(&side[2]),]
    );
    assert_eq!(seqs(&outcomes), (1..=7).collect::<Vec<_>>());
    // The log from genesis wrote the same records after its first three, less the golden keys.
    let from_genesis = full_log.outcomes();
    assert_eq!(from_genesis.len(), 3 + outcomes.len());
    for (late, full) in outcomes.iter().zip(&from_genesis[3..]) {
        assert_eq!(shapes(std::slice::from_ref(late)), shapes(std::slice::from_ref(full)));
        if late.kind() == Kind::Connect {
            assert_eq!(keys(late.s()), C_KEYS[..12], "no golden key");
            for key in &C_KEYS[..12] {
                assert_eq!(late.s().get(key), full.s().get(key), "{key}");
            }
        } else {
            assert_eq!(late.s(), full.s());
        }
        assert_eq!(late.o(), full.o());
    }
}

/// `TestBlockValidity(m, check_pow, check_merkle)` through V.
fn propose(w: &World, m: &Mined, pow: bool, merkle: bool) -> Result<BlockState, NotTested> {
    w.rig.v.test_block_validity(m.block.clone(), pow, merkle).recv().expect("V replies")
}

fn invalid(state: Result<BlockState, NotTested>) -> BlockValidationError {
    match state {
        Ok(BlockState::Invalid(err)) => err,
        other => panic!("{other:?}, want Invalid"),
    }
}

/// CH3a D125 (O8 §4.3, §8.2): a proposal's `R`, of origin `proposal`, only for an attempt whose
/// `ConnectBlock` ran (step 4, `v:4979`): a ChainLock conflict (step 1, `v:4948-4951`) or a
/// pre-check (step 3, `v:4963-4978`; all at v24.0.0) records nothing. A pass is `res:"ok"`. Tip and
/// proposal attempts share one `seq`.
#[test]
fn a_proposal_records_an_r_only_when_connect_block_ran() {
    let (mut w, log) = logged(Setup {
        locked_at: Some(1),
        ..Setup::default()
    });
    let g = w.rig.genesis.clone();
    let b1 = block_on(&w.rig.p, &g, vec![]);
    assert_eq!(invalid(propose(&w, &b1, false, true)).reject_reason(), "bad-chainlock");
    w.rig.stop();
    assert_eq!(shapes(&log.outcomes()), [c(&g)], "step 1");

    let (mut w, log) = logged(Setup::default());
    let p = w.rig.p.clone();
    let b1 = block_on(&p, &g, vec![]);
    w.rig.accept(&b1);
    let b2 = block_on(&p, &b1, vec![]);
    let unmerkled = Mined {
        block: Arc::new(Block::new(*b2.block.header(), vec![coinbase_tx(2, 1, 0, 2 * COIN)])),
        height: 2,
    };
    assert_eq!(invalid(propose(&w, &unmerkled, false, true)).reject_reason(), "bad-txnmrklroot");
    let overpaid = with_coinbase(&p, &b1, coinbase_tx(2, 1, 0, 1_000_000 * COIN), vec![]);
    let err = invalid(propose(&w, &overpaid, false, true));
    assert_eq!(err.reject_reason(), "bad-cb-amount");
    assert_eq!(propose(&w, &b2, false, true), Ok(BlockState::Valid));
    w.rig.accept(&b2);
    w.rig.stop();

    let outcomes = log.outcomes();
    assert_eq!(
        shapes(&outcomes),
        [c(&g), c(&b1), r(&overpaid, Origin::Proposal), r(&b2, Origin::Proposal), c(&b2)]
    );
    assert_eq!(seqs(&outcomes), [1, 2, 3, 4, 5]);
    let (bad, ok) = (outcomes[2].s(), outcomes[3].s());
    assert_eq!(bad.get("res"), Some(&Value::string("invalid")));
    assert_eq!(bad.get("cls"), Some(&Value::string(class_name(err.result()))));
    assert_eq!(bad.get("why"), Some(&Value::string("bad-cb-amount")));
    assert_eq!(outcomes[2].x().get("dbg"), Some(&dbg(&err)));
    assert_eq!(ok.get("res"), Some(&Value::string("ok")));
    assert_eq!(ok.get("cls"), Some(&Value::Null));
    assert_eq!(ok.get("why"), Some(&Value::Null));
    assert_eq!(outcomes[3].x().get("dbg"), Some(&Value::Null));
}

/// CH3a D137 (O8 §8.3; O8's patch, `AttemptScope::Guard` and `AttemptScope::Failed`): a failed
/// record write fails the recording only. It is an `AbortNode`, which trips the latch and starts
/// the node's shutdown, and the attempt goes on as Core's does: a `C`'s block commits
/// (`v:3446-3448`), an `R`'s is marked `FAILED_VALID` (`v:3434-3435`), a `D`'s disconnect flushes
/// (`v:3285-3287`), and a proposal answers its verdict. V then stops on the latch, with the write's
/// text reported once: after a `C` or a `D` at the tip step's IF_NEEDED site (`v:3457`, `:3304`),
/// before its tip moves; after an `R` at ActivateBestChain's PERIODIC site, once
/// `ShutdownRequested()` has ended its loop (`v:3841`, `:3851`); after a proposal at its next
/// flush, here its stop's. The log writes nothing after the failure (O8's patch, `Fail`). The `H`
/// is written as the log opens.
#[test]
fn a_failed_record_write_fails_only_the_recording() {
    let failing = |lines| Log {
        lines: Some(lines),
        ..Log::default()
    };
    assert!(DigestLog::new(failing(0), header(FileKind::Digest)).is_err(), "the H");
    for case in ["C", "R", "D", "proposal"] {
        // The `H` and genesis's `C` are written, and for the `D` the `C`s of b1 and b2.
        let log = match case {
            "D" => failing(4),
            "proposal" => Log {
                once: true,
                ..failing(2)
            },
            _ => failing(2),
        };
        let (mut w, log) = logged_to(log, Setup::default());
        let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
        let (tip, written) = match case {
            "C" | "R" => {
                let txs = match case {
                    "C" => vec![],
                    _ => vec![spend(&[seed_op(9)], COIN)],
                };
                let b1 = block_on(&p, &g, txs);
                w.rig.submit_stopping(&b1, case);
                let (state, status) = (checked(&w.rig), w.rig.status(&b1));
                if case == "C" {
                    assert_eq!(state, [(b1.hash(), BlockState::Valid)]);
                    assert_eq!(coins_at(&w.coins), Some(b1.hash()), "committed");
                    assert!(status.has(Status::HAVE_UNDO), "{status:?}");
                    assert!(!status.has(Status::FAILED_MASK), "{status:?}");
                } else {
                    assert!(matches!(state[..], [(h, BlockState::Invalid(_))] if h == b1.hash()));
                    assert_eq!(coins_at(&w.coins), Some(g.hash()));
                    assert!(status.has(Status::FAILED_VALID), "{status:?}");
                }
                (g.clone(), vec![c(&g)])
            }
            "D" => {
                let b1 = block_on(&p, &g, vec![]);
                let b2 = block_on(&p, &b1, vec![]);
                let side = branch(&p, &b1, 2, 1);
                w.rig.accept(&b1);
                w.rig.accept(&b2);
                w.rig.headers(&side);
                w.rig.accept(&side[0]);
                w.rig.submit_stopping(&side[1], case);
                assert_eq!(coins_at(&w.coins), Some(b1.hash()), "flushed");
                (b2.clone(), vec![c(&g), c(&b1), c(&b2)])
            }
            _ => {
                // The first proposal's write fails and the second's would not: the log writes
                // nothing after a failure.
                let b1 = block_on(&p, &g, vec![]);
                for n in 0..2 {
                    assert_eq!(propose(&w, &b1, false, true), Ok(BlockState::Valid), "{n}");
                }
                (g.clone(), vec![c(&g)])
            }
        };
        let latched = w.seen.lock().expect("seen").latch.tripped().map(|f| f.message.clone());
        let exit = w.rig.stop();
        let text = fatal_text(&exit).expect("an AbortNode");
        assert!(text.starts_with("statedigest: writing the digest log"), "{case}: {text}");
        assert_eq!(w.sink.messages(), [text], "{case}: one report");
        assert_eq!(latched.as_deref(), Some(text), "{case}: the latch");
        assert!(w.rig.shutdown.shutdown_requested(), "{case}: the node's shutdown");
        let at = exit.chain.private_tip().map(|(t, _)| t.hash);
        assert_eq!(at, Some(tip.hash()), "{case}: the tip");
        assert_eq!(shapes(&log.outcomes()), written, "{case}");
    }
}

/// CH3a D137: the `C` is written as `ConnectBlock` returns, before the commit, as O8's patch
/// writes it before `ConnectTip`'s flush (`v:3446-3448`): a failed commit after it stops V with
/// the `C` in the log.
#[test]
fn a_failed_commit_follows_its_c() {
    let (mut w, log) = logged(Setup {
        steps: StepsCfg {
            fail_tx: Some(TxStep::Commit),
            ..StepsCfg::default()
        },
        ..Setup::default()
    });
    let g = w.rig.genesis.clone();
    let b1 = block_on(&w.rig.p, &g, vec![]);
    w.rig.submit_stopping(&b1, "the commit fails");
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(TX_FAILED));
    assert_eq!(shapes(&log.outcomes()), [c(&g), c(&b1)]);
}

/// CH3a D138: a `Fatal` out of `ConnectBlock` is Core's exception out of it (O8's patch,
/// `AttemptScope::Threw`): `R` `error` with `why` `exception` and the text as `dbg`.
#[test]
fn a_fault_in_connect_block_records_an_exception() {
    let (mut w, log) = logged(Setup {
        steps: StepsCfg {
            complete: true,
            fail_h8_at: Some(1),
            ..StepsCfg::default()
        },
        extra: Some(Deployment::Versionbits(DeploymentPos::V24)),
        seeds: vec![seed(1, 50 * COIN, &[OP_TRUE])],
        ..Setup::default()
    });
    let g = w.rig.genesis.clone();
    let b1 = block_on(&w.rig.p, &g, vec![spend(&[seed_op(1)], 49 * COIN)]);
    w.rig.submit_stopping(&b1, "H8 fails");
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(H8_FAILED));
    let outcomes = log.outcomes();
    assert_eq!(shapes(&outcomes), [c(&g), r(&b1, Origin::Tip)]);
    let rec = &outcomes[1];
    assert_eq!(rec.s().get("res"), Some(&Value::string("error")));
    assert_eq!(rec.s().get("cls"), Some(&Value::Null));
    assert_eq!(rec.s().get("why"), Some(&Value::string("exception")));
    assert_eq!(rec.x().get("dbg"), Some(&Value::string(H8_FAILED)));
}

// ReplayChainHelper (CH3 §3.3, O8 §7.2).

/// An IS lock's or transaction's hash in display order (O8 §7.1).
fn wire(bytes: &[u8; 32]) -> DHash {
    DHash::from_wire_bytes(*bytes)
}

/// The adjusted time of the recording run's attempts.
const RECORDED_AT: i64 = 1_700_000_000;

/// A recording: its digest log and its decisions file's records (`H`, then one `A` per `C` or
/// `R`, paired by `seq`).
struct Recording {
    digest: Vec<u8>,
    decisions: Vec<Record>,
}

impl Recording {
    fn decisions(&self) -> Arc<RecordedDecisions> {
        let decisions: Vec<u8> = self.decisions.iter().flat_map(Record::to_line).collect();
        Arc::new(RecordedDecisions::load(&self.digest, &decisions).expect("the recording loads"))
    }

    /// The `A` of the attempt `n` (counting from genesis's).
    fn attempt(&mut self, n: usize) -> &mut Attempt {
        match &mut self.decisions[n + 1] {
            Record::Attempt(a) => a,
            other => panic!("{other:?}"),
        }
    }
}

/// The scenario both runs make: an IS setup with lock A and a ChainLock; `b1` spends seed 1 (lock
/// A's input, CH3 §3.3 `[A, null]`), a proposal of `b2` passes, then `b2` connects and `b3` is
/// invalid. Returns the blocks.
fn scenario(w: &World) -> [Mined; 3] {
    let (p, g) = (&w.rig.p, &w.rig.genesis);
    let b1 = block_on(p, g, vec![spend(&[seed_op(1)], COIN)]);
    let b2 = block_on(p, &b1, vec![spend(&[seed_op(2)], COIN)]);
    let b3 = block_on(p, &b2, vec![spend(&[seed_op(9)], COIN)]);
    w.rig.accept(&b1);
    assert_eq!(propose(w, &b2, false, true), Ok(BlockState::Valid));
    w.rig.accept(&b2);
    assert!(matches!(last_state(w, std::slice::from_ref(&b3)), BlockState::Invalid(_)));
    [b1, b2, b3]
}

/// The scenario recorded through [`RecordingHelper`] over the scripted helper, its `A`s written
/// by [`TapedLog`] to a decisions file: one per `C` and `R`, with its record's identity and `seq`
/// and the clock's time, genesis's with no query.
fn record() -> (Recording, Vec<Effect>) {
    let (log, mut file) = (Log::default(), Log::default());
    file.write_all(&Record::Header(header(FileKind::Decisions)).to_line()).expect("the H");
    let tap = DecisionTap::new();
    let helper_tap = tap.clone();
    let clock = Arc::new(dash_clock::MockClock::new());
    clock.set_mocktime(RECORDED_AT);
    let inner = DigestLog::new(log.clone(), header(FileKind::Digest)).expect("the H");
    let mut w = world(Setup {
        digest: Some(Box::new(TapedLog::new(inner, file.clone(), tap))),
        node_stop: true,
        wrap_helper: Some(Box::new(move |inner| {
            Box::new(RecordingHelper::new(inner, helper_tap, clock))
        })),
        ..is_setup(vec![lock_a()], true)
    });
    scenario(&w);
    let exit = w.rig.stop();
    assert!(exit.fatal.is_none());
    let effects = std::mem::take(&mut w.seen.lock().expect("seen").effects);
    let bytes = file.bytes();
    let decisions: Vec<Record> = Lines::new(&bytes).map(|(_, r)| r.expect("a record")).collect();
    let attempted: Vec<Outcome> =
        log.outcomes().into_iter().filter(|o| o.kind() != Kind::Disconnect).collect();
    assert_eq!(attempted.len() + 1, decisions.len(), "one A per C and R");
    for (o, a) in attempted.iter().zip(&decisions[1..]) {
        let Record::Attempt(a) = a else {
            panic!("{a:?} in the decisions");
        };
        assert_eq!(a.id, o.id);
        assert_eq!(a.x().get("seq"), o.x().get("seq"));
        assert_eq!(a.t, RECORDED_AT);
    }
    let Record::Attempt(genesis) = &decisions[1] else {
        panic!("genesis's A");
    };
    assert!(genesis.q.is_empty(), "genesis's attempt asks nothing (v:2534-2540): an empty q");
    let recording = Recording {
        digest: log.bytes(),
        decisions,
    };
    (recording, effects)
}

/// A replay of the scenario's setup with a [`ReplayChainHelper`] over `decisions`, and its log.
fn replay(
    decisions: Arc<RecordedDecisions>,
    policy: Policy,
    choice: AttemptChoice,
) -> (World, Log, MissSlot) {
    let miss = MissSlot::new();
    let slot = miss.clone();
    let (w, log) = logged(Setup {
        wrap_helper: Some(Box::new(move |_| {
            Box::new(ReplayChainHelper::new(decisions, policy, choice, slot))
        })),
        ..is_setup(vec![lock_a()], true)
    });
    (w, log, miss)
}

/// O8 §7.2: a strict and a keyed replay of a recording through V make the same attempts with the
/// same outcomes: the replay's log is the recording's, byte for byte, and its effects are too.
#[test]
fn strict_and_keyed_replays_repeat_the_recording() {
    let (recording, effects) = record();
    let decisions = recording.decisions();
    for (policy, choice) in
        [(Policy::Strict, AttemptChoice::FinalConnect), (Policy::Keyed, AttemptChoice::InOrder)]
    {
        let (mut w, log, miss) = replay(decisions.clone(), policy, choice);
        scenario(&w);
        let exit = w.rig.stop();
        assert!(exit.fatal.is_none(), "{policy:?}: {:?}", exit.fatal);
        assert_eq!(miss.get(), None, "{policy:?}");
        assert!(log.bytes() == recording.digest, "{policy:?}: the logs differ");
        assert_eq!(w.seen.lock().expect("seen").effects, effects, "{policy:?}");
    }
}

/// CH3 D35: after `cl_conflict`, the recorded `[A, null]` loop is `ibd`, `cl_has` and exactly two
/// `is_conflict` answers (`v:2801-2819`), the superblock queries after them; the removal between
/// them took no entry, and a strict replay asks for exactly those two (see
/// [`strict_and_keyed_replays_repeat_the_recording`]).
/// With one more `is_conflict` appended, the strict replay ends `b1`'s attempt with a recorded
/// query unasked: the miss stops the replay at its next helper call, `b2`'s proposal.
#[test]
fn the_d35_cursor_takes_two_is_conflict_answers_for_a_removal() {
    let (mut recording, effects) = record();
    let a = lock_a();
    assert_eq!(effects, [removal(&a)]);
    let b1 = recording.attempt(1);
    let txid = match &b1.q[3] {
        Query::IsConflict {
            txid,
            ..
        } => *txid,
        other => panic!("{other:?}"),
    };
    let pair = IsConflictPair {
        islock: wire(a.r.hash.as_bytes()),
        conflicting_txid: wire(a.r.txid.as_bytes()),
    };
    let is_conflict = |conflict| Query::IsConflict {
        txid,
        conflict,
    };
    let (height, hash) = (1, b1.id.b);
    let head = [
        Query::ClConflict {
            height,
            hash,
            answer: false,
        },
        Query::Ibd(false),
        Query::ClHas {
            height,
            hash,
            answer: true,
        },
    ];
    assert_eq!(b1.q[..3], head);
    assert_eq!(b1.q[3..5], [is_conflict(Some(pair)), is_conflict(None)]);
    let asked = b1.q.iter().filter(|q| matches!(q, Query::IsConflict { .. })).count();
    assert_eq!(asked, 2, "{:?}", b1.q);

    b1.q.push(is_conflict(None));
    let (mut w, _, miss) =
        replay(recording.decisions(), Policy::Strict, AttemptChoice::FinalConnect);
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![spend(&[seed_op(1)], COIN)]);
    w.rig.accept(&b1);
    let text = miss.get().expect("a miss");
    assert!(text.ends_with("1 recorded queries were not asked"), "{text}");
    assert_eq!(w.seen.lock().expect("seen").effects, [removal(&a)]);
    let b2 = block_on(&p, &b1, vec![spend(&[seed_op(2)], COIN)]);
    w.rig.submit_stopping(&b2, "the replay stopped");
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(text.as_str()));
}

/// O8 §7.2: a replay never guesses. A query the recording does not have in that place (strict:
/// out of order), or an attempt it does not have, is a decision miss: the replay stops with the
/// miss's text, which the slot keeps.
#[test]
fn a_decision_miss_stops_the_replay() {
    let (mut recording, _) = record();
    // `b1`'s `ibd` and `cl_has` swapped.
    recording.attempt(1).q.swap(1, 2);
    let (mut w, _, miss) =
        replay(recording.decisions(), Policy::Strict, AttemptChoice::FinalConnect);
    let b1 = block_on(&w.rig.p, &w.rig.genesis, vec![spend(&[seed_op(1)], COIN)]);
    w.rig.submit_stopping(&b1, "out of order");
    let exit = w.rig.stop();
    let text = miss.get().expect("a miss");
    assert!(text.starts_with("decision miss: attempt"), "{text}");
    assert!(text.contains("query 1 is Ibd"), "{text}");
    assert_eq!(fatal_text(&exit), Some(text.as_str()));

    // A block the recording never connected.
    for choice in [AttemptChoice::FinalConnect, AttemptChoice::InOrder] {
        let (recording, _) = record();
        let (mut w, _, miss) = replay(recording.decisions(), Policy::Keyed, choice);
        let other = block_on(&w.rig.p, &w.rig.genesis, vec![]);
        w.rig.submit_stopping(&other, &format!("{choice:?}"));
        let exit = w.rig.stop();
        let text = miss.get().expect("a miss");
        assert!(text.starts_with("decision miss:"), "{choice:?}: {text}");
        assert!(text.contains(&hash(&other).to_string()), "{choice:?}: {text}");
        assert_eq!(fatal_text(&exit), Some(text.as_str()), "{choice:?}");
    }
}

/// O8 §5.2's evo keys, which hook H11 gives a `C` (CH7a-5b).
const EVO_KEYS: [&str; 4] = ["mnl", "qc", "cp", "ehf"];

/// The scripted H11's fields at `height` ([`h11_fields`]) as the `C` record's values, every hash
/// in display order.
fn h11_values(height: u32) -> [(&'static str, Value); 4] {
    let h = |n: u8| Value::hash(&DHash::from_wire_bytes(spread(n)));
    let row = |t: i64, q: u8, c: u8| Value::Arr(vec![Value::Int(t), h(q), h(c)]);
    [
        ("mnl", h(h11_mnl_seed(height))),
        ("qc", Value::Arr(vec![row(1, 0x01, 0x21), row(100, 0x41, 0x61)])),
        ("cp", Value::Arr(vec![Value::Int(7), Value::Int(6), Value::Int(2), h(0xa1)])),
        ("ehf", Value::Arr(vec![Value::Arr(vec![Value::Int(10), Value::Int(5)])])),
    ]
}

/// The evo keys a `C` holds, with their values.
fn evo_of(c: &Outcome) -> Vec<(&'static str, Value)> {
    EVO_KEYS.iter().filter_map(|&k| c.s().get(k).map(|v| (k, v.clone()))).collect()
}

/// Complete hooks under test, as [`StepsCfg`] sets them.
fn complete(steps: StepsCfg) -> Setup {
    Setup {
        steps: StepsCfg {
            complete: true,
            ..steps
        },
        ..Setup::default()
    }
}

/// How often H11 ran.
fn h11_calls(w: &World) -> usize {
    w.seen.lock().expect("seen").calls.iter().filter(|c| matches!(c, Call::DigestFields)).count()
}

/// CH7a-5b X1/X4: with complete hooks, a `C` carries H11's four keys with H11's values, each in
/// the record table's place (`Fields::set` keeps table order), right after `undo`. Genesis's `C` has H11's genesis answer, every key `null`.
#[test]
fn a_complete_hook_sets_c_records_evo_keys_in_table_order() {
    let (mut w, log) = logged(complete(StepsCfg::default()));
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![]);
    w.rig.accept(&b1);
    w.rig.stop();
    let outcomes = log.outcomes();
    assert_eq!(shapes(&outcomes), [c(&g), c(&b1)]);
    let undo = C_KEYS.iter().position(|&k| k == "undo").expect("undo") + 1;
    let mut want: Vec<&str> = C_KEYS[..undo].to_vec();
    want.extend(EVO_KEYS);
    want.extend(&C_KEYS[undo..]);
    assert_eq!(keys(outcomes[1].s()), want);
    assert_eq!(evo_of(&outcomes[1]), h11_values(1));
    let nulls: Vec<_> = EVO_KEYS.iter().map(|&k| (k, Value::Null)).collect();
    assert_eq!(evo_of(&outcomes[0]), nulls, "genesis");
    assert_eq!(h11_calls(&w), 2);
}

/// F4 (CH3a D144): with incomplete hooks (`NoEvo`'s answers) the connector calls no H11 and a
/// `C` has none of the evo keys: absent, never `null`.
#[test]
fn noevo_leaves_the_evo_keys_absent() {
    let (mut w, log) = logged(Setup::default());
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![]);
    w.rig.accept(&b1);
    w.rig.stop();
    let outcomes = log.outcomes();
    assert_eq!(shapes(&outcomes), [c(&g), c(&b1)]);
    for o in &outcomes {
        assert_eq!(keys(o.s()), C_KEYS);
    }
    assert_eq!(h11_calls(&w), 0);
}

/// X1: without a sink the connector calls no H11, complete hooks or not.
#[test]
fn no_sink_makes_no_h11_call() {
    let mut w = world(complete(StepsCfg::default()));
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![]);
    w.rig.accept(&b1);
    w.rig.stop();
    assert_eq!(h11_calls(&w), 0);
}

/// X3: H11's failure fails the record, not the attempt: one `AbortNode` with its text, the block
/// connected and committed, and nothing written after genesis's `C`.
#[test]
fn a_failed_h11_fails_the_record_and_keeps_the_outcome() {
    let (mut w, log) = logged(complete(StepsCfg {
        h11_fails: true,
        ..StepsCfg::default()
    }));
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![]);
    w.rig.submit_stopping(&b1, "H11");
    assert_eq!(checked(&w.rig), [(b1.hash(), BlockState::Valid)]);
    assert_eq!(coins_at(&w.coins), Some(b1.hash()), "committed");
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(H11_FAILED));
    assert_eq!(w.sink.messages(), [H11_FAILED], "one report");
    assert_eq!(shapes(&log.outcomes()), [c(&g)]);
}

/// X1 (Sol r1 #4): the body connects and H11 runs, then `finish_attempt` fails: the outcome is
/// that failure, so the record is the attempt's `R`; H11's values reach no record, the pending
/// transaction is rolled back once and never committed.
#[test]
fn a_failed_finish_after_h11_writes_the_failed_attempts_r_and_no_c() {
    let (mut w, log) = logged(complete(StepsCfg {
        fail_finish_after_h11: true,
        ..StepsCfg::default()
    }));
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![]);
    let before = w.seen.lock().expect("seen").calls.len();
    w.rig.submit_stopping(&b1, "finish");
    let calls: Vec<_> = w.seen.lock().expect("seen").calls[before..].to_vec();
    w.rig.stop();
    let outcomes = log.outcomes();
    assert_eq!(shapes(&outcomes), [c(&g), r(&b1, Origin::Tip)]);
    let bytes = String::from_utf8(log.bytes()).expect("utf-8");
    let mnl = spread(h11_mnl_seed(1));
    let wire: String = mnl.iter().map(|b| format!("{b:02x}")).collect();
    let shown: String = mnl.iter().rev().map(|b| format!("{b:02x}")).collect();
    assert!(!bytes.contains(&wire) && !bytes.contains(&shown), "no H11 value in the log");
    assert!(evo_of(&outcomes[1]).is_empty(), "no evo key in the R");
    let count = |c: &Call| {
        calls.iter().filter(|x| std::mem::discriminant(*x) == std::mem::discriminant(c)).count()
    };
    assert_eq!(count(&Call::DigestFields), 1);
    assert_eq!(count(&Call::RollbackTx), 1);
    assert_eq!(count(&Call::CommitTx), 0);
}

/// X1: `TestBlockValidity` makes no H11 call, nor a connect that failed.
#[test]
fn no_h11_for_a_just_check_or_a_failed_connect() {
    let (mut w, log) = logged(complete(StepsCfg::default()));
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let calls = h11_calls(&w);
    let b1 = block_on(&p, &g, vec![]);
    assert_eq!(propose(&w, &b1, false, true), Ok(BlockState::Valid));
    assert_eq!(h11_calls(&w), calls, "a just-check");
    let bad = block_on(&p, &g, vec![spend(&[seed_op(9)], COIN)]);
    last_invalid(&w, std::slice::from_ref(&bad));
    assert_eq!(h11_calls(&w), calls, "a failed connect");
    w.rig.stop();
    let outcomes = log.outcomes();
    assert_eq!(outcomes.len(), 3, "genesis's C and two Rs");
    assert!(outcomes[1..].iter().all(|o| evo_of(o).is_empty()));
}

/// X1, CH3 §5.4: H11 runs inside the attempt, after the body and before `finish_attempt`.
#[test]
fn h11_runs_before_the_attempt_finishes() {
    let (mut w, _log) = logged(complete(StepsCfg::default()));
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![]);
    let before = w.seen.lock().expect("seen").calls.len();
    w.rig.accept(&b1);
    let calls: Vec<_> = w.seen.lock().expect("seen").calls[before..].to_vec();
    w.rig.stop();
    let at = |c: Call| {
        calls.iter().position(|x| std::mem::discriminant(x) == std::mem::discriminant(&c))
    };
    let (begin, h11, finish) =
        (at(Call::BeginAttempt), at(Call::DigestFields), at(Call::FinishAttempt));
    assert!(begin < h11 && h11 < finish, "{calls:?}");
    assert!(begin.is_some() && h11.is_some(), "{calls:?}");
}

/// X1 on VerifyDB's scratch target: a level-4 reconnect's `org:"verifydb"` `C` carries H11's
/// keys, as its tip `C` does. The chain is below every G1 rule, so the walk reaches level 4; the
/// hooks are the scripted ones, so this checks the record's plumbing, not a staging read
/// (CH7a-5b's `scratch_state_hash` reads real staging).
#[test]
fn a_verifydb_c_record_carries_the_evo_keys() {
    let (mut w, log) = logged(complete(StepsCfg::default()));
    let (p, g) = (w.rig.p.clone(), w.rig.genesis.clone());
    let b1 = block_on(&p, &g, vec![]);
    let b2 = block_on(&p, &b1, vec![]);
    w.rig.accept(&b1);
    w.rig.accept(&b2);
    let verified = w
        .rig
        .v
        .verify_db(VerifyDbArgs {
            level: 4,
            depth: 2,
        })
        .recv()
        .expect("V answers");
    assert_eq!(verified, VerifyDbResult::Success);
    w.rig.stop();
    let outcomes = log.outcomes();
    let verifydb: Vec<_> = outcomes
        .iter()
        .filter(|o| o.kind() == Kind::Connect && o.id.org == Origin::Verifydb)
        .collect();
    assert_eq!(verifydb.len(), 2, "{:?}", shapes(&outcomes));
    for o in verifydb {
        let tip = outcomes
            .iter()
            .find(|t| t.kind() == Kind::Connect && t.id.org == Origin::Tip && t.id.b == o.id.b)
            .expect("its tip C");
        assert_eq!(evo_of(o), h11_values(u32::try_from(o.id.h).expect("a height")));
        assert_eq!(evo_of(o), evo_of(tip));
    }
}

// ReplayChainHelper's `cl_best` (CH7b-3 C6; X7).

/// A block hash with no two bytes alike, so a byte-order slip shows.
fn hash_of(n: u8) -> BlockHash {
    BlockHash::from(std::array::from_fn(|i| n.wrapping_add(u8::try_from(i).expect("< 32"))))
}

/// The lock `cl_best` returns for the recorded best ChainLock `best_at(height, n)`.
fn lock_at(height: i32, n: u8) -> ChainLockSig {
    ChainLockSig {
        height,
        block_hash: hash_of(n),
        sig: std::array::from_fn(|i| n ^ u8::try_from(i).expect("< 96")),
    }
}

/// The best ChainLock at `height` on block `n`, as O8 records a `cl_best` answer.
fn best_at(height: i32, n: u8) -> Query {
    let lock = lock_at(height, n);
    Query::ClBest(Some(BestChainLock {
        height: height.into(),
        hash: display(lock.block_hash).expect("hex"),
        sig: lock.sig,
    }))
}

/// A recording whose attempts each ask `cl_best`: block 7 at the tip twice, the first attempt
/// failing (best lock at 5 on block 1, then at 6 on block 2); a proposal of block 8 (at 7 on
/// block 3); block 9 at the tip with Core's null sig.
fn cl_recording() -> Arc<RecordedDecisions> {
    let id = |h: i64, n: u8, org| Ident {
        h,
        b: display(hash_of(n)).expect("hex"),
        org,
    };
    let outcome = |mut o: Outcome, seq: i64| {
        o.x_mut().set("seq", Value::Int(seq)).expect("seq");
        Record::Outcome(o)
    };
    let attempt = |ident: Ident, seq: i64, q: Query| {
        let mut a = Attempt::new(ident, 0);
        a.q = vec![q];
        a.x_mut().set("seq", Value::Int(seq)).expect("seq");
        Record::Attempt(a)
    };
    let mut failed = Outcome::reject(id(7, 7, Origin::Tip));
    failed.s_mut().set("res", Value::string("invalid")).expect("res");
    let outcomes = [
        Record::Header(header(FileKind::Digest)),
        outcome(failed, 1),
        outcome(Outcome::connect(id(7, 7, Origin::Tip)), 2),
        outcome(Outcome::connect(id(8, 8, Origin::Proposal)), 3),
        outcome(Outcome::connect(id(9, 9, Origin::Tip)), 4),
    ];
    let decisions = [
        Record::Header(header(FileKind::Decisions)),
        attempt(id(7, 7, Origin::Tip), 1, best_at(5, 1)),
        attempt(id(7, 7, Origin::Tip), 2, best_at(6, 2)),
        attempt(id(8, 8, Origin::Proposal), 3, best_at(7, 3)),
        attempt(id(9, 9, Origin::Tip), 4, Query::ClBest(None)),
    ];
    let bytes =
        |records: &[Record]| -> Vec<u8> { records.iter().flat_map(Record::to_line).collect() };
    Arc::new(RecordedDecisions::load(&bytes(&outcomes), &bytes(&decisions)).expect("it loads"))
}

/// What a strict [`ReplayChainHelper`] over `decisions` under `choice` answers `cl_best` in each
/// of `attempts` (block `n`, origin), each begun, asked once and ended in turn inside V.
fn cl_best_answers(
    decisions: Arc<RecordedDecisions>,
    choice: AttemptChoice,
    attempts: Vec<(u8, AttemptOrigin)>,
) -> Vec<Result<Option<ChainLockSig>, String>> {
    let rig = Rig::new();
    let out = Arc::new(Mutex::new(None));
    let seen = out.clone();
    rig.on_v(move |v| {
        let mut helper = ReplayChainHelper::new(decisions, Policy::Strict, choice, MissSlot::new());
        let answers = with_tip_cx(v, |_, tip| {
            let mut ask = |n: u8, origin| -> Result<Option<ChainLockSig>, Fatal> {
                let cx = ConnectCx {
                    origin,
                    ..*tip
                };
                let at = BlockAt {
                    hash: hash_of(n),
                    height: n.into(),
                    prev: None,
                    id: None,
                };
                helper.begin_attempt(&cx, &at)?;
                let best = helper.cl_best(&cx);
                helper.end_attempt();
                Ok(best?.map(|b| ChainLockSig::clone(&b)))
            };
            let answers = attempts.into_iter().map(|(n, origin)| ask(n, origin));
            answers.map(|r| r.map_err(|f| format!("{f:?}"))).collect()
        });
        *seen.lock().expect("seen") = Some(answers);
        Ok(())
    });
    let answers = out.lock().expect("out").take();
    answers.expect("V ran it")
}

/// CH7b-3 C6 (R-1): `cl_best` answers the attempt's recorded best ChainLock, its height, the
/// block hash read from the record's display form and the signature's bytes, for a tip attempt
/// (the final connect's) and a proposal; Core's null sig is `None`.
#[test]
fn cl_best_answers_from_the_recording() {
    let attempts =
        vec![(7, AttemptOrigin::Tip), (8, AttemptOrigin::Proposal), (9, AttemptOrigin::Tip)];
    let answers = cl_best_answers(cl_recording(), AttemptChoice::FinalConnect, attempts);
    assert_eq!(answers, [Ok(Some(lock_at(6, 2))), Ok(Some(lock_at(7, 3))), Ok(None)]);
}

/// CH7b-3 C6 (R-2; X7): each answer stays in its attempt. Replayed in order, block 7's failed
/// first attempt answers its own lock and the second attempt its own, so a failed attempt's
/// answer never reaches the next; the proposal after them answers its own.
#[test]
fn cl_best_answers_stay_in_their_attempt() {
    let tip = AttemptOrigin::Tip;
    let attempts = vec![(7, tip), (7, tip), (8, AttemptOrigin::Proposal)];
    let answers = cl_best_answers(cl_recording(), AttemptChoice::InOrder, attempts);
    let want = [Ok(Some(lock_at(5, 1))), Ok(Some(lock_at(6, 2))), Ok(Some(lock_at(7, 3)))];
    assert_eq!(answers, want);
}

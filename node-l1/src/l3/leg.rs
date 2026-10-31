//! One L3 leg (plan §3.2, §3.3): a dashd and V on the same chain arguments, the setup chain
//! both build, each case's trials on both, and the planes after every op. Every op that changes
//! a node is logged with dashd's reply and V's step, for the shim replay ([`super::replay`]).
//! The planes' decisions are [`super::planes`]'s; the leg fetches what they read.
//!
//! The leg stops at its first harness error or divergence, which its log names; nothing after
//! it is scored.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use dash_oracle_sys::{ChainOp, Pin, Post, RpcError};
use serde_json::{Value, json};

use dashd_chainstate::evo::gates;

use super::dashd::{Process, is_synced};
use super::evo;
use super::inventory::{Binding, Effects, Facts, Inventory, Reach};
use super::ledger::{self, Ledger, Mempool, Removal, is_coinbase, muhash_refs};
use super::model::Model;
use super::planes::{self, View, block_at, rpc_hash};
use super::trial::{CaseId, Kind, Owner, TrialId, TxidLedger, from_hex, hex, to_hex};
use super::version::{Dashd, private_dir};
use crate::chain::build::{self, BlockPlan, Lock, push};
use crate::chain::mutate::{Cx, Expect, L3Cx, Mode, Operator, Par, Site, Variant, catalogue};
use crate::chain::node::{NodeSide, index_digest};
use crate::chain::params;
use crate::chain::shape::Outcome;
use crate::chain::step::Step;
use crate::chain::world::{Coin, Hash, header_hash};

/// The legs' first clock; height `h`'s is `T0 + 150·h` (plan §3.2).
pub const T0: i64 = 1_700_000_000;
/// The height of the first case: every case's coins are mature there, every boundary passed.
pub const CASE_BASE: u32 = 201;
/// D49's arguments, then the boundary heights (main legs, Q2).
pub const CHAIN_ARGS: [&str; 8] = [
    "-dip3params=2000:2000",
    "-testactivationheight=v20@2000",
    "-testactivationheight=mn_rr@2000",
    "-testactivationheight=dip0001@120",
    "-testactivationheight=dersig@130",
    "-testactivationheight=cltv@140",
    "-testactivationheight=csv@150",
    "-testactivationheight=bip147@160",
];
/// The boundary heights: P6 runs on both sides of each.
pub const BOUNDARIES: [u32; 5] = [120, 130, 140, 150, 160];

/// `RPC_INVALID_ADDRESS_OR_KEY`: an unknown block.
const UNKNOWN_BLOCK: i64 = -5;
/// A raw type-5 transaction (the coinbase type CbTx's step 4 skips at `vtx[0]`).
const TRANSACTION_COINBASE: u16 = 5;

/// A leg's chain arguments: the main legs' and its `-par`.
pub fn chain_args(par: Par) -> Vec<String> {
    CHAIN_ARGS.iter().copied().chain([par.arg()]).map(str::to_owned).collect()
}

/// A case of the run.
#[derive(Clone, Copy)]
pub struct Case {
    pub id: CaseId,
    pub op: &'static Operator,
    pub variant: &'static Variant,
}

impl Case {
    /// Whether the case is positive: its cold submit is accepted, so it has a rollback and a
    /// T-seq (plan §3.2).
    pub fn positive(&self) -> bool {
        (self.variant.expect)(Mode::Submit, self.id.par) == Expect::Accept
    }

    pub fn kinds(&self) -> &'static [Kind] {
        if self.positive() {
            &[Kind::Cold, Kind::Prop, Kind::Seq]
        } else {
            &[Kind::Cold, Kind::Prop]
        }
    }
}

/// tier 1's cases on a leg (a10-4: every operator's variants at `Reg`), in manifest order, which
/// fixes each case's height and funding whatever the filter.
pub fn cases(par: Par) -> Vec<Case> {
    let mut out = Vec::new();
    for op in catalogue() {
        for variant in op.variants {
            for &site in variant.sites.iter().filter(|s| **s == Site::Reg) {
                out.push(Case {
                    id: CaseId {
                        op: op.name,
                        variant: variant.name,
                        site,
                        par,
                    },
                    op,
                    variant,
                });
            }
        }
    }
    out
}

/// One op both nodes ran.
#[derive(Clone, Debug)]
pub struct Logged {
    pub op: ChainOp,
    pub trial: Option<TrialId>,
    /// dashd's reply (`SETTIME`'s is null).
    pub dashd: Outcome,
    pub v: Step,
    /// The model's best header after the op, where P7 ran on it (the replay holds Core to it).
    pub model_best_header: Option<(u32, Hash)>,
}

/// A trial's record.
#[derive(Clone, Debug)]
pub struct TrialRecord {
    pub trial: TrialId,
    pub hash: Hash,
    /// The credited op's index in the log.
    pub op: usize,
    pub mode: Mode,
    pub reply: Outcome,
    /// `None` for an accept or an RPC refusal (no occurrence).
    pub binding: Option<Binding>,
    /// The bound row's class (§3.5).
    pub bound_class: Option<String>,
    /// V's class: its `BlockChecked`'s (submit) or its TBV state's (proposal).
    pub v_class: Option<String>,
    pub effects: Effects,
    /// §3.6's per-body observations, with the credited op's E-evo delta (M-3).
    pub evo: Value,
    /// Disconnected transactions dashd's ATMP declined at the rollback, with its reason.
    pub declined: Listed,
    /// Transactions the submitted block took from dashd's mempool, with their reason
    /// (OD-L3-21: `confirmed`, `conflict on <outpoint>`, `descendant of <txid>`).
    pub removed: Listed,
}

/// A leg's result.
#[derive(Debug)]
pub struct LegLog {
    pub name: String,
    pub par: Par,
    pub chain_args: Vec<String>,
    pub dashd_argv: Vec<String>,
    pub ops: Vec<Logged>,
    pub trials: Vec<TrialRecord>,
    /// Up posts whose candidate set held more than one entry (OD-L3-19), by the model: V's
    /// `candidates` arrive with track N's accessors, Core's at the ABI 33 pin.
    pub multi_candidate_up_posts: u64,
    /// V's load status at construction (OD-L3-16): `None` once it serves.
    pub load_refused: Option<String>,
    /// V's E-evo counters after the last op (§3.6).
    pub evo_counts: BTreeMap<&'static str, u64>,
    /// What the planes read after each trial op, when the run records (`L3_RECORD`): the
    /// witnesses' fixtures, with Core's post added by the replay.
    pub recorded: Vec<Value>,
    /// The leg's wall time.
    pub seconds: f64,
    /// The leg's first harness error or divergence; `None` for a leg done.
    pub error: Option<String>,
}

/// What a leg runs.
pub struct LegConfig<'a> {
    pub dashd: &'a Dashd,
    pub par: Par,
    pub inventory: &'a Inventory,
    /// Run only the cases whose id or operator is this (plan §3.2's filtered run).
    pub only: Option<&'a str>,
    /// Record what the planes read after each trial op ([`LegLog::recorded`]).
    pub record: bool,
}

/// Run a leg in a new directory under `dir`.
pub fn run(config: &LegConfig<'_>, dir: &Path) -> LegLog {
    run_on(config, dir, chain_args(config.par))
}

/// [`run`] on the given chain arguments.
fn run_on(config: &LegConfig<'_>, dir: &Path, chain_args: Vec<String>) -> LegLog {
    let mut log = LegLog {
        name: config.par.name().to_owned(),
        par: config.par,
        chain_args,
        dashd_argv: Vec::new(),
        ops: Vec::new(),
        trials: Vec::new(),
        multi_candidate_up_posts: 0,
        load_refused: None,
        evo_counts: BTreeMap::new(),
        recorded: Vec::new(),
        seconds: 0.0,
        error: None,
    };
    let start = std::time::Instant::now();
    let mut leg = match Leg::open(config, dir, &mut log) {
        Ok(leg) => leg,
        Err(e) => {
            log.error = Some(e);
            return log;
        }
    };
    if let Err(e) = leg.chain(&mut log) {
        log.error = Some(e);
    }
    if let Err(e) = leg.node.take().map_or(Ok(()), Process::stop) {
        log.error.get_or_insert(e);
    }
    log.seconds = start.elapsed().as_secs_f64();
    log
}

struct Leg<'a> {
    config: &'a LegConfig<'a>,
    node: Option<Process>,
    v: NodeSide,
    model: Model,
    ledger: Ledger,
    mempool: Mempool,
    txids: TxidLedger,
    cases: Vec<Case>,
    /// Each setup block's coinbase coin, by height.
    funding: BTreeMap<u32, Coin>,
    mocktime: i64,
    height: u32,
    /// V's last post.
    post: Post,
}

fn err<T>(what: impl Into<String>) -> Result<T, String> {
    Err(what.into())
}

/// The E-evo counters' change over one op.
type Delta = BTreeMap<&'static str, u64>;

/// Transactions by txid, each with a reason.
pub type Listed = Vec<(String, String)>;

/// The trial's own transactions: every one but a coinbase at `vtx[0]` (a block without one has
/// none to skip).
pub fn own_txids(plan: &BlockPlan, txids: &[Hash]) -> Vec<Hash> {
    let skip = usize::from(plan.txs.first().is_some_and(is_coinbase));
    txids.iter().skip(skip).copied().collect()
}

/// The rollback's mempool delta (Q-G, OD-L3-21): dashd gained exactly the disconnected own
/// transactions it re-admitted, and lost only what the submitted block took (`removed`: the
/// baseline mempool's removals for the block, with their reasons), each listed with its
/// reason. Anything else is a harness error.
pub fn mempool_delta(
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
    own: &[Hash],
    removed: &BTreeMap<Hash, Removal>,
) -> Result<Listed, String> {
    let disconnected: BTreeSet<String> = own.iter().map(hex).collect();
    let added: Vec<&String> = after.difference(before).collect();
    if let Some(stray) = added.iter().find(|t| !disconnected.contains(**t)) {
        return err(format!(
            "the rollback's mempool gained {stray}, not a rolled-back transaction"
        ));
    }
    let reasons: BTreeMap<String, &Removal> = removed.iter().map(|(id, r)| (hex(id), r)).collect();
    before
        .difference(after)
        .map(|lost| match reasons.get(lost) {
            Some(reason) => Ok((lost.clone(), reason.to_string())),
            None => err(format!("the rollback's mempool lost {lost}, which the block neither confirms nor conflicts with")),
        })
        .collect()
}

/// The E-evo hook's delta a body must give (M-3): `d.stx_hook` once per transaction, `w.stx`
/// once per transaction step 4 enters (all but a raw type-5 `vtx[0]`) for a body past step 4,
/// none before it, and between one and all inside it. A dead hook gives none for a body that
/// must count.
pub fn liveness(plan: &BlockPlan, reach: Reach, delta: &Delta) -> Result<(), String> {
    let n = plan.txs.len() as u64;
    let skipped = u64::from(plan.txs.first().is_some_and(|t| t.tx_type == TRANSACTION_COINBASE));
    let (hook, wrapped) = (delta.get("d.stx_hook").copied(), delta.get("w.stx").copied());
    let ok = match reach {
        Reach::None => hook == Some(0) && wrapped == Some(0),
        Reach::Full => hook == Some(n) && wrapped == Some(n - skipped),
        Reach::Partial => wrapped.is_some_and(|w| w >= 1 && w <= n - skipped) && hook >= wrapped,
    };
    if !ok {
        return err(format!(
            "§3.6 E-evo liveness: a {n}-transaction body with reach {reach:?} gave d.stx_hook \
             {hook:?}, w.stx {wrapped:?} (the E-evo hook is not live, or counts what V did not run)"
        ));
    }
    Ok(())
}

/// The variant's expectation against dashd's reply and its binding (the harness's
/// self-check): an accept or an RPC refusal binds nothing; a reject has the reason's prefix
/// and binds exactly the declared occurrence.
pub fn self_check(want: &Expect, reply: &Outcome, binding: &Option<Binding>) -> Result<(), String> {
    let ok = match (want, reply, binding) {
        (Expect::Accept, Ok(Value::Null), None) => true,
        (
            Expect::Reject {
                reason,
                occurrence,
            },
            Ok(Value::String(s)),
            Some(Binding::Bound(key)),
        ) => s.starts_with(reason) && key == occurrence,
        (
            Expect::Refused {
                code,
                message,
            },
            Err(e),
            None,
        ) => e.code == Some(*code) && e.message == *message,
        _ => false,
    };
    if !ok {
        return err(format!(
            "operator drift: dashd replied {reply:?}, bound {binding:?}; the variant expects {want:?}"
        ));
    }
    Ok(())
}

/// The derived `g.mnhf` (§3.6, m-5): a body with an MNHF signal is refused, with a unique
/// binding strictly before ProcessSpecialTxsInBlock's step 11 (Core's MNHF ProcessBlock), so
/// the gated body never ran. An extraction error is recorded as such: Core's own extraction
/// fails it there too.
pub fn mnhf_gate(
    inventory: &Inventory,
    signals: &Result<usize, String>,
    accepted: bool,
    binding: &Option<Binding>,
) -> Result<(), String> {
    if !matches!(signals, Ok(n) if *n > 0) {
        return Ok(());
    }
    let before = match binding {
        Some(Binding::Bound(key)) => {
            inventory.row(key).is_some_and(|r| inventory.before_mnhf_step(r))
        }
        _ => false,
    };
    if accepted || !before {
        return err(format!(
            "§3.6 g.mnhf: a body with MNHF signals was not refused before step 11 (bound {binding:?}, \
             accepted {accepted}); V's site: {}",
            evo::site("g.mnhf")
        ));
    }
    Ok(())
}

/// §3.6's per-body gates, as the leg asserted them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyGates {
    /// `extract_signals`' count, or its error.
    pub mnhf_signals: Result<usize, String>,
    /// What the gates read: `getdeploymentinfo` at the parent on dashd and V's post there,
    /// and dashd's decode of the first transaction.
    pub dashd_parent: Value,
    pub v_parent: Value,
    pub first_decoded: Value,
}

impl BodyGates {
    pub fn json(&self) -> Value {
        let mnhf = match &self.mnhf_signals {
            Ok(n) => json!(n),
            Err(e) => json!(format!("extract_error({e})")),
        };
        json!({"dip0003": false, "v24": false, "cbtx": false, "mnhf_signals": mnhf})
    }
}

impl<'a> Leg<'a> {
    /// dashd and V on the leg's arguments, after D8's check of V's parameters against Core's
    /// (before any chain session), and V's load checked (OD-L3-16: fail closed).
    fn open(config: &'a LegConfig<'a>, dir: &Path, log: &mut LegLog) -> Result<Leg<'a>, String> {
        let dir = private_dir(dir, &log.name)?;
        let v = NodeSide::with_argv(&log.chain_args, T0, &private_dir(&dir, "v")?, None)?;
        if let Some(message) = v.down() {
            log.load_refused = Some(message.to_owned());
            return err(format!("V's load was refused: {message}"));
        }
        evo::check(evo::EVO_KEYS, &config.inventory.evo_keys, &gates::KEYS)?;
        // The chain-parameter options only: `-par` is the node's.
        params::check_args(Pin::V24_0, &log.name, &CHAIN_ARGS, false, v.params())?;
        let datadir = private_dir(&dir, "dashd")?;
        let node = Process::start(&config.dashd.binary, &datadir, &log.chain_args, T0)?;
        log.dashd_argv = node.argv().to_vec();
        if log.dashd_argv.iter().any(|a| a.starts_with("-coinstatsindex")) {
            return err("P5 needs dashd without -coinstatsindex");
        }
        let mut leg = Leg {
            config,
            node: Some(node),
            v,
            model: Model::new(build::genesis_hash()),
            ledger: Ledger::default(),
            mempool: Mempool::default(),
            txids: TxidLedger::default(),
            cases: cases(config.par),
            funding: BTreeMap::new(),
            mocktime: T0,
            height: 0,
            post: Post {
                down: true,
                flush_delta: Default::default(),
                tie: Vec::new(),
                persisted: None,
                tip: None,
                best_header: None,
                utxo: None,
                index: None,
                flushes: None,
                ibd: None,
                best_cl: None,
                sporks: None,
                tips: None,
                deployments: None,
            },
        };
        let chain = leg.rpc("getblockchaininfo", json!([]))?;
        if chain["chain"] != "regtest" || chain["blocks"] != 0 {
            return err(format!("dashd did not start at regtest genesis: {chain}"));
        }
        // The genesis post, for P6 at leg start.
        leg.step(ChainOp::SetTime(T0), None, Ok(Value::Null), log)?;
        leg.p6()?;
        Ok(leg)
    }

    fn node(&self) -> &Process {
        self.node.as_ref().expect("dashd runs")
    }

    fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        planes::chain_only(method, &params)?;
        self.node().rpc().ok(method, params)
    }

    /// An RPC as the op's reply: its result, or its error in the shim's shape.
    fn reply(&self, method: &str, params: Value) -> Result<Outcome, String> {
        Ok(self.node().rpc().call(method, params)?.map_err(|e| RpcError {
            code: Some(e.code),
            message: e.message,
        }))
    }

    /// V's step for an op dashd ran (with `dashd`'s reply), logged, then P1; V down is a
    /// harness error. Also V's class for a proposal and the op's E-evo delta.
    fn step(
        &mut self,
        op: ChainOp,
        trial: Option<TrialId>,
        dashd: Outcome,
        log: &mut LegLog,
    ) -> Result<(Step, Option<&'static str>, Delta), String> {
        let s = self.v.step(&op).map_err(|u| format!("V refused {}: {u:?}", u.kind))?;
        if let Some(message) = self.v.down() {
            return err(format!("V went down: {message}"));
        }
        // §3.6: after every op, no gated body entered on V.
        let counts = self.v.evo_gates().ok_or("V's E-evo counters are not registered")?;
        evo::gate_check(&counts, log.ops.len())?;
        // A count that went back is a second registration hiding the live one (M-3).
        let delta: Delta = counts
            .iter()
            .map(|(k, n)| {
                let was = log.evo_counts.get(k).copied().unwrap_or_default();
                n.checked_sub(was).map(|d| (*k, d)).ok_or_else(|| {
                    format!(
                        "§3.6: V's E-evo counter {k} went from {was} to {n} at op {}",
                        log.ops.len()
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        log.evo_counts = counts;
        self.post = s.step.post.clone();
        log.ops.push(Logged {
            op,
            trial,
            dashd,
            v: s.step.clone(),
            model_best_header: None,
        });
        let last = log.ops.last().expect("an op ran");
        planes::p1(log.ops.len() - 1, &last.dashd, &last.v.reply.outcome)?;
        Ok((s.step, s.proposal_class, delta))
    }

    /// The setup chain (plan §3.2): per height the clock, the case there, then the setup block.
    fn chain(&mut self, log: &mut LegLog) -> Result<(), String> {
        let last = CASE_BASE + self.cases.len() as u32;
        for h in 1..=last {
            self.mocktime = T0 + 150 * i64::from(h);
            self.rpc("setmocktime", json!([self.mocktime]))?;
            self.step(ChainOp::SetTime(self.mocktime), None, Ok(Value::Null), log)?;
            if let Some(i) = h.checked_sub(CASE_BASE).filter(|i| (*i as usize) < self.cases.len()) {
                let case = self.cases[i as usize];
                if self.selected(&case) {
                    for &kind in case.kinds() {
                        self.trial(&case, i, kind, log)?;
                    }
                }
            }
            self.setup_block(h, log)?;
        }
        Ok(())
    }

    fn selected(&self, case: &Case) -> bool {
        self.config.only.is_none_or(|only| only == case.id.op || only == case.id.to_string())
    }

    /// `generateblock raw(51) [] true` on dashd, its bytes to V.
    fn setup_block(&mut self, h: u32, log: &mut LegLog) -> Result<(), String> {
        let prev = self.model.tip();
        let made = self.rpc("generateblock", json!(["raw(51)", [], true]))?;
        let hash = rpc_hash(&made["hash"])?;
        let hex_block = self.rpc("getblock", json!([hex(&hash), 0]))?;
        let bytes = from_hex(hex_block.as_str().ok_or("getblock: not hex")?)?;
        let plan = BlockPlan::from_block(&bytes)?;
        let txids: Vec<Hash> = plan.txs.iter().map(|t| t.txid()).collect();
        let head: [u8; 80] = bytes[..80].try_into().expect("a header");
        if header_hash(&head) != hash || plan.prev != prev || plan.txs.len() != 1 {
            return err(format!("setup block {h}: not one coinbase on the tip"));
        }
        self.body_gates(&prev, &plan)?;
        let (_, _, delta) = self.step(ChainOp::Block(bytes), None, Ok(Value::Null), log)?;
        liveness(&plan, Reach::Full, &delta).map_err(|e| format!("setup block {h}: {e}"))?;
        self.model.connect(hash, prev);
        self.ledger.connect(&plan, &txids, h)?;
        self.mempool.remove_for_block(&plan, &txids);
        self.height = h;
        let cb = &plan.txs[0];
        if cb.outputs.len() != 1 || cb.outputs[0].1 != Lock::True.script_pubkey() {
            return err(format!("setup block {h}: its coinbase is not one OP_TRUE output"));
        }
        self.funding.insert(
            h,
            Coin {
                outpoint: (txids[0], 0),
                value: cb.outputs[0].0,
                lock: Lock::True,
                height: h,
                coinbase: true,
            },
        );
        self.planes(log)?;
        let pool = self.rpc("getrawmempool", json!([]))?;
        if mempool_set(&pool)? != self.mempool.txids().iter().map(hex).collect() {
            return err(format!(
                "after setup block {h}: dashd's mempool {pool} is not the model's"
            ));
        }
        if BOUNDARIES.iter().any(|b| h + 1 == *b || h == *b) || h.is_multiple_of(144) {
            self.p6()?;
        }
        Ok(())
    }

    /// One trial (plan §3.2): its block from a fresh template, its manifest, the op on both
    /// nodes, the planes, its binding, and a positive submit's rollback.
    fn trial(&mut self, case: &Case, i: u32, kind: Kind, log: &mut LegLog) -> Result<(), String> {
        let h = self.height + 1;
        let prev = self.model.tip();
        let trial = TrialId {
            case: case.id,
            kind,
        };
        let template = self.rpc("generateblock", json!(["raw(51)", [], false]))?;
        let mut plan =
            BlockPlan::from_block(&from_hex(template["hex"].as_str().ok_or("no template hex")?)?)?;
        if plan.txs.len() != 1 || plan.prev != prev {
            return err(format!("{trial}: the template is not one coinbase on the tip"));
        }
        if i64::from(plan.time) != self.mocktime {
            return err(format!("{trial}: the template's time is not the clock"));
        }
        let mtp = self.rpc("getblockheader", json!([hex(&prev)]))?["mediantime"]
            .as_u64()
            .and_then(|t| u32::try_from(t).ok())
            .ok_or("getblockheader: no mediantime")?;
        let coins: Vec<Coin> = (1..=3)
            .map(|k| {
                self.funding
                    .get(&(3 * i + k))
                    .copied()
                    .ok_or(format!("no funding at {}", 3 * i + k))
            })
            .collect::<Result<_, _>>()?;
        let immature = *self.funding.get(&(h - 99)).ok_or("no immature coin")?;
        let l3 = L3Cx {
            trial: trial.bytes(),
            carrier: case.variant.carrier,
        };
        let cx = Cx {
            height: h,
            mtp,
            mocktime: self.mocktime,
            coins: &coins,
            immature: Some(immature),
            l3: Some(&l3),
        };
        (case.op.apply)(&mut plan, &cx);
        if is_coinbase(&plan.txs[0]) {
            plan.txs[0].inputs[0].script_sig.extend(push(&trial.bytes()));
        }
        let built = plan.build();
        let own = own_txids(&plan, &built.txids);
        self.txids.record(Owner::Trial(trial.to_string()), &own, &[])?;
        let mut all_coins = coins.clone();
        all_coins.push(immature);
        self.manifest(&trial, h, &built.hash, &all_coins)?;
        let measured = ledger::measure(&plan, &built.txids, h, mtp, &self.ledger);
        let gates = self.body_gates(&prev, &plan)?;
        let parent = self.model.parent_state(&prev);
        let baseline = (self.ledger.clone(), self.mempool.clone(), self.mempool_now()?);

        if kind == Kind::Seq {
            // Warm on purpose: its own proposal first, credited as the sequence case only.
            let (reply, _, delta) = self.propose(&built.bytes, trial, log)?;
            if reply != Ok(Value::Null) {
                return err(format!(
                    "{trial}: the sequence case's proposal was refused: {reply:?}"
                ));
            }
            liveness(&plan, Reach::Full, &delta).map_err(|e| format!("{trial}: {e}"))?;
        }
        let mode = kind.mode();
        let before = self.effects(&built.hash)?;
        let (reply, v_class, delta) = match mode {
            Mode::Proposal => self.propose(&built.bytes, trial, log)?,
            _ => {
                let dashd = self.reply("submitblock", json!([to_hex(&built.bytes)]))?;
                let (step, _, delta) = self.step(
                    ChainOp::Block(built.bytes.clone()),
                    Some(trial),
                    dashd.clone(),
                    log,
                )?;
                let class = step
                    .reply
                    .checked
                    .iter()
                    .find(|c| c.0.0 == built.hash)
                    .and_then(|c| c.2.clone());
                (dashd, class.map(|c| c.to_owned()), delta)
            }
        };
        let op = log.ops.len() - 1;
        let effects = self.effects(&built.hash)?;
        let accepted = reply == Ok(Value::Null);
        if mode == Mode::Proposal && effects != before {
            return err(format!("{trial}: a proposal changed the index: {before:?} → {effects:?}"));
        }
        if mode == Mode::Submit {
            self.observe_submit(&plan, &built.hash, &built.txids, &prev, effects, accepted, h)?;
        }
        let facts = Facts {
            network: self.rpc("getblockchaininfo", json!([]))?["chain"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            // §3.6's gates hold for every body the leg submits (`body_gates` above).
            dip3: false,
            cbtx: false,
            mnsync_synced: is_synced(&self.rpc("mnsync", json!(["status"]))?)?,
            par: case.id.par,
            parent,
            measured,
        };
        let inventory = self.config.inventory;
        let binding = match &reply {
            Ok(Value::String(reason)) => Some(inventory.bind(
                reason,
                mode,
                &facts,
                (mode == Mode::Submit).then_some(effects),
            )),
            _ => None,
        };
        let want = (case.variant.expect)(mode, case.id.par);
        self_check(&want, &reply, &binding).map_err(|e| format!("{trial}: {e}"))?;
        let bound_row = match &binding {
            Some(Binding::Bound(key)) => Some(inventory.row(key).expect("a bound row")),
            _ => None,
        };
        let bound_class = bound_row.map(|row| inventory.class(row));
        // M-3: the body's E-evo delta, from where it was refused.
        let reach = match (accepted, bound_row) {
            (true, _) => Reach::Full,
            (false, Some(row)) => inventory.reach(row),
            (false, None) => Reach::None,
        };
        liveness(&plan, reach, &delta).map_err(|e| format!("{trial}: {e}"))?;
        mnhf_gate(inventory, &gates.mnhf_signals, accepted, &binding)
            .map_err(|e| format!("{trial}: {e}"))?;
        let view = self.planes(log)?;
        let mut evo = gates.json();
        evo["delta"] = json!({"d.stx_hook": delta.get("d.stx_hook"), "w.stx": delta.get("w.stx")});
        self.record(log, &trial, kind.name(), op, &built.hash, &prev, &view, &gates)?;
        let mut record = TrialRecord {
            trial,
            hash: built.hash,
            op,
            mode,
            reply,
            binding,
            bound_class,
            v_class,
            effects,
            evo,
            declined: Vec::new(),
            removed: Vec::new(),
        };
        if accepted && mode == Mode::Submit {
            let (declined, removed) = self.rollback(
                &plan,
                &built.hash,
                &built.txids,
                &own,
                &prev,
                &gates,
                baseline,
                trial,
                log,
            )?;
            record.declined = declined;
            record.removed = removed;
        }
        log.trials.push(record);
        Ok(())
    }

    fn propose(
        &mut self,
        bytes: &[u8],
        trial: TrialId,
        log: &mut LegLog,
    ) -> Result<(Outcome, Option<String>, Delta), String> {
        let dashd =
            self.reply("getblocktemplate", json!([{"mode": "proposal", "data": to_hex(bytes)}]))?;
        let (_, class, delta) =
            self.step(ChainOp::Proposal(bytes.to_vec()), Some(trial), dashd.clone(), log)?;
        Ok((dashd, class.map(str::to_owned), delta))
    }

    /// The model after a submit, from dashd's effects (rev1 §3.3's path table), which V's must
    /// equal.
    #[allow(clippy::too_many_arguments)]
    fn observe_submit(
        &mut self,
        plan: &BlockPlan,
        hash: &Hash,
        txids: &[Hash],
        prev: &Hash,
        effects: Effects,
        accepted: bool,
        h: u32,
    ) -> Result<(), String> {
        match (effects.known, effects.stored, effects.failed) {
            (false, false, false) => {}
            (true, true, false) if accepted => {
                self.model.connect(*hash, *prev);
                self.ledger.connect(plan, txids, h)?;
                self.mempool.remove_for_block(plan, txids);
            }
            (true, true, failed) => self.model.connect_reject(*hash, *prev, !failed),
            (true, false, failed) => self.model.contextual_reject(*hash, *prev, !failed),
            other => return err(format!("submit effects outside the path table: {other:?}")),
        }
        Ok(())
    }

    /// A positive submit's rollback (plan §3.3 P7): `invalidateblock`, the planes, the coins back
    /// at the baseline, and dashd's mempool delta (Q-G, OD-L3-21): it gained the block's own
    /// transactions only, and lost only what the block took, each listed with its reason.
    #[allow(clippy::too_many_arguments)]
    fn rollback(
        &mut self,
        plan: &BlockPlan,
        hash: &Hash,
        txids: &[Hash],
        own: &[Hash],
        prev: &Hash,
        gates: &BodyGates,
        (ledger, mempool, before): (Ledger, Mempool, BTreeSet<String>),
        trial: TrialId,
        log: &mut LegLog,
    ) -> Result<(Listed, Listed), String> {
        let dashd = self.reply("invalidateblock", json!([hex(hash)]))?;
        self.step(ChainOp::Invalidate(*hash), Some(trial), dashd, log)?;
        let op = log.ops.len() - 1;
        self.model.invalidate_tip(*hash);
        self.ledger = ledger;
        let after = self.mempool_now()?;
        let taken = mempool.removed_for_block(plan, txids);
        let removed =
            mempool_delta(&before, &after, own, &taken).map_err(|e| format!("{trial}: {e}"))?;
        self.mempool = mempool;
        for (id, _) in &removed {
            self.mempool.txs.retain(|t, _| hex(t) != *id);
        }
        let mut declined = Vec::new();
        for (tx, id) in plan.txs.iter().map(|t| (t, t.txid())).filter(|(_, id)| own.contains(id)) {
            if after.contains(&hex(&id)) {
                self.mempool.txs.insert(id, tx.clone());
            } else {
                let raw = to_hex(&dash_consensus::core_serialize(&*tx.tx()));
                let r = self.rpc("testmempoolaccept", json!([[raw]]))?;
                let reason = r[0]["reject-reason"].as_str().unwrap_or("accepted").to_owned();
                declined.push((hex(&id), reason));
            }
        }
        if self.mempool.txids().iter().map(hex).collect::<BTreeSet<_>>() != after {
            return err(format!("{trial}: after the rollback, dashd's mempool is not the model's"));
        }
        let effects = self.effects(hash)?;
        if effects
            != (Effects {
                known: true,
                failed: true,
                stored: true,
            })
        {
            return err(format!("{trial}: rollback effects {effects:?}"));
        }
        let view = self.planes(log)?;
        self.record(log, &trial, "rollback", op, hash, prev, &view, gates)?;
        Ok((declined, removed))
    }

    fn mempool_now(&self) -> Result<BTreeSet<String>, String> {
        mempool_set(&self.rpc("getrawmempool", json!([]))?)
    }

    /// The per-trial manifest (plan §3.2), before the trial: a mismatch is a harness error.
    fn manifest(
        &mut self,
        trial: &TrialId,
        h: u32,
        hash: &Hash,
        coins: &[Coin],
    ) -> Result<(), String> {
        let prev = self.model.tip();
        let fail = |what: &str| err(format!("{trial}: manifest: {what}"));
        if rpc_hash(&self.rpc("getbestblockhash", json!([]))?)? != prev
            || self.post.tip != Some(block_at(h - 1, prev))
        {
            return fail("the tip is not the parent on both sides");
        }
        if self.rpc("getdeploymentinfo", json!([hex(&prev)]))?
            != self.post.deployments.clone().unwrap_or_default()
        {
            return fail("the parent's deployments differ");
        }
        match self.node().rpc().call("getblockheader", json!([hex(hash)]))? {
            Err(e) if e.code == UNKNOWN_BLOCK => {}
            other => return fail(&format!("the trial's block is known to dashd: {other:?}")),
        }
        if self.v_entry(hash)?.is_some() || self.model.knows(hash) {
            return fail("the trial's block is known to V");
        }
        let v_coins = self.v_outpoints()?;
        for c in coins {
            let out = self.rpc("gettxout", json!([hex(&c.outpoint.0), c.outpoint.1, false]))?;
            if out.is_null()
                || !v_coins.contains(&c.outpoint)
                || !self.ledger.coins.contains_key(&c.outpoint)
            {
                return fail(&format!(
                    "funding {}:{} is not unspent chain-only",
                    hex(&c.outpoint.0),
                    c.outpoint.1
                ));
            }
        }
        planes::mnsync_unsynced(&self.rpc("mnsync", json!(["status"]))?).or_else(|e| fail(&e))?;
        let ibd = self.rpc("getblockchaininfo", json!([]))?["initialblockdownload"].as_bool();
        if ibd != Some(false) || self.post.ibd != Some(false) {
            return fail("a side is in IBD");
        }
        Ok(())
    }

    /// V's index entry for `hash`: its status.
    fn v_entry(&self, hash: &Hash) -> Result<Option<u32>, String> {
        let entries = self.v.index().ok_or("V's index is not readable")?;
        Ok(entries.iter().find(|e| e.hash.as_bytes() == hash).map(|e| e.status))
    }

    fn v_outpoints(&self) -> Result<BTreeSet<(Hash, u32)>, String> {
        let coins = self
            .v
            .tip_coins()
            .ok_or("V serves no coins")?
            .map_err(|e| format!("V's coins: {e:?}"))?;
        Ok(coins
            .iter()
            .map(|(k, _)| {
                let o = k.outpoint();
                (*o.hash.as_bytes(), o.index)
            })
            .collect())
    }

    /// What dashd shows of `hash`: whether `getblockheader` knows it and `getblock` serves it.
    fn dashd_known_stored(&self, hash: &Hash) -> Result<(bool, bool), String> {
        let rpc = self.node().rpc();
        let known = match rpc.call("getblockheader", json!([hex(hash)]))? {
            Ok(_) => true,
            Err(e) if e.code == UNKNOWN_BLOCK => false,
            Err(e) => return err(format!("getblockheader: {e:?}")),
        };
        let stored = known && rpc.call("getblock", json!([hex(hash), 0]))?.is_ok();
        Ok((known, stored))
    }

    /// P2: `hash`'s effects on dashd (`getblockheader`, `getchaintips`, `getblock <h> 0`), which
    /// V's index must show too.
    fn effects(&self, hash: &Hash) -> Result<Effects, String> {
        let (known, stored) = self.dashd_known_stored(hash)?;
        let tips = if known {
            self.rpc("getchaintips", json!([]))?
        } else {
            Value::Null
        };
        let dashd = planes::dashd_effects(known, stored, &tips, hash)?;
        planes::p2(hash, dashd, self.v_entry(hash)?)?;
        Ok(dashd)
    }

    /// What P3 and P5 read from dashd.
    fn view(&self) -> Result<View, String> {
        Ok(View {
            tips: self.rpc("getchaintips", json!([]))?,
            best: self.rpc("getbestblockhash", json!([]))?,
            info: self.rpc("getblockchaininfo", json!([]))?,
            txoutset: self.rpc("gettxoutsetinfo", json!(["muhash"]))?,
        })
    }

    fn v_muhash(&self) -> Result<String, String> {
        let coins = self
            .v
            .tip_coins()
            .ok_or("V serves no coins")?
            .map_err(|e| format!("V's coins: {e:?}"))?;
        Ok(hex(&muhash_refs(coins.iter().map(|(k, c)| (k.outpoint(), c.as_ref())))))
    }

    /// P3, P5 and P7 after an op (the model updated): their reads, decided by [`planes`].
    fn planes(&mut self, log: &mut LegLog) -> Result<View, String> {
        let at = log.ops.len() - 1;
        let view = self.view()?;
        let model_header = self.model.best_header();
        planes::p3(at, &view, &self.post, model_header.0)?;
        planes::p5(&view, &self.model.tip(), &self.v_muhash()?, &hex(&self.ledger.muhash()))?;
        planes::p7(&self.post, model_header)?;
        log.ops.last_mut().expect("an op ran").model_best_header = Some(model_header);
        if !self.post.down && self.model.candidates().len() > 1 {
            log.multi_candidate_up_posts += 1;
        }
        Ok(view)
    }

    /// P6: `getdeploymentinfo` at the tip vs V's post.
    fn p6(&self) -> Result<(), String> {
        planes::p6(self.height, &self.rpc("getdeploymentinfo", json!([]))?, &self.post)
    }

    /// When the run records: what the planes read after the trial's op `op`, for the
    /// witnesses' fixtures (the replay adds Core's post at `op`).
    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        log: &mut LegLog,
        trial: &TrialId,
        what: &str,
        op: usize,
        hash: &Hash,
        parent: &Hash,
        view: &View,
        gates: &BodyGates,
    ) -> Result<(), String> {
        if !self.config.record {
            return Ok(());
        }
        let (known, stored) = self.dashd_known_stored(hash)?;
        let entries = self.v.index().ok_or("V's index is not readable")?;
        let (dashd_reply, v_reply) = (
            planes::outcome_json(&log.ops[op].dashd),
            planes::outcome_json(&log.ops[op].v.reply.outcome),
        );
        let model = self.model.best_header();
        log.recorded.push(json!({
            "trial": trial.to_string(),
            "what": what,
            "op": op,
            "hash": hex(hash),
            "parent": hex(parent),
            "dashd": {
                "reply": dashd_reply,
                "known": known,
                "stored": stored,
                "view": view.json(),
                "getdeploymentinfo": self.rpc("getdeploymentinfo", json!([]))?,
                "getdeploymentinfo_parent": gates.dashd_parent,
                "first_decoded": gates.first_decoded,
                "mnsync": self.rpc("mnsync", json!(["status"]))?,
            },
            "v": {
                "reply": v_reply,
                "status": self.v_entry(hash)?,
                "muhash": self.v_muhash()?,
                "index": entries.iter().map(|e| json!([to_hex(e.hash.as_bytes()), e.height, e.status, e.has_txs])).collect::<Vec<_>>(),
                "index_digest": to_hex(&index_digest(&entries).0),
                "deployments_parent": gates.v_parent,
                "evo_counts": log.evo_counts,
            },
            "ledger_muhash": hex(&self.ledger.muhash()),
            "model_best_header": [model.0, hex(&model.1)],
            "model_tip": hex(&self.model.tip()),
        }));
        Ok(())
    }

    /// §3.6's per-body gates for a block on `parent`: DIP3 and V24 inactive for it on both
    /// sides, no decoded CbTx, and the MNHF signals `extract_signals` finds (an extraction error
    /// recorded as such, never as none). A failed gate is a harness error.
    fn body_gates(&self, parent: &Hash, plan: &BlockPlan) -> Result<BodyGates, String> {
        let dashd = self.rpc("getdeploymentinfo", json!([hex(parent)]))?;
        let v = self.post.deployments.clone().unwrap_or_default();
        let first = to_hex(&dash_consensus::core_serialize(&*plan.txs[0].tx()));
        let first_decoded = self.rpc("decoderawtransaction", json!([first]))?;
        planes::body_gates(parent, &dashd, &v, &first_decoded)?;
        let built = plan.build();
        let mut cursor = &built.bytes[..];
        let block = dash_consensus::decode_block(&mut cursor)
            .map_err(|e| format!("§3.6: the body does not decode: {e:?}"))?
            .into_value();
        Ok(BodyGates {
            mnhf_signals: dash_dml::mnhf::extract_signals(&block)
                .map(|bits| bits.len())
                .map_err(|e| e.reject_reason().to_string()),
            dashd_parent: dashd,
            v_parent: v,
            first_decoded,
        })
    }
}

fn mempool_set(pool: &Value) -> Result<BTreeSet<String>, String> {
    pool.as_array()
        .ok_or_else(|| format!("getrawmempool: {pool}"))?
        .iter()
        .map(|t| t.as_str().map(str::to_owned).ok_or_else(|| format!("getrawmempool: {t}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::build::{Input, TxPlan, coinbase};

    /// A block of `txs`, and their txids.
    fn plan_of(txs: Vec<TxPlan>) -> (BlockPlan, Vec<Hash>) {
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

    /// tier 1 has every operator's `Reg` variants once per leg, with ids as plan §3.2 writes
    /// them; the one positive case is the control (m-8), which has a T-seq.
    #[test]
    fn tier1_has_the_catalogue_at_reg() {
        let par1 = cases(Par::One);
        assert_eq!(par1.len(), catalogue().map(|o| o.variants.len()).sum::<usize>());
        assert_eq!(par1[0].id.to_string(), "cb_amount.reg@Reg/par1");
        let positive: Vec<String> =
            par1.iter().filter(|c| c.positive()).map(|c| c.id.to_string()).collect();
        assert_eq!(positive, ["control.reg@Reg/par1"]);
        assert!(par1.iter().all(|c| c.kinds().len()
            == if c.positive() {
                3
            } else {
                2
            }));
        let ids: BTreeSet<String> = par1.iter().map(|c| c.id.to_string()).collect();
        assert_eq!(ids.len(), par1.len(), "unique case ids");
        // Each case's coins are mature at its height: the last funding height + 100 ≤ its height.
        let last = par1.len() as u32 - 1;
        assert!(3 * last + 3 + build::MATURITY <= CASE_BASE + last);
        assert!(BOUNDARIES.iter().all(|b| *b < CASE_BASE));
        assert_eq!(chain_args(Par::Four).last().map(String::as_str), Some("-par=4"));
    }

    /// n-6: one self-check of reply and binding against the variant.
    #[test]
    fn the_self_check_reads_replies_and_bindings_as_expectations() {
        let key = "validation.cpp:ConnectBlock#5:bad-txns-nonfinal";
        let reject = Expect::Reject {
            reason: "bad-txns-nonfinal",
            occurrence: key,
        };
        let bound = Some(Binding::Bound(key.into()));
        assert_eq!(self_check(&reject, &Ok(Value::from("bad-txns-nonfinal")), &bound), Ok(()));
        assert!(self_check(&reject, &Ok(Value::Null), &None).is_err());
        let other =
            Some(Binding::Bound("validation.cpp:ContextualCheckBlock#2:bad-txns-nonfinal".into()));
        assert!(self_check(&reject, &Ok(Value::from("bad-txns-nonfinal")), &other).is_err());
        let ambiguous = Some(Binding::Ambiguous(vec![key.into()]));
        assert!(self_check(&reject, &Ok(Value::from("bad-txns-nonfinal")), &ambiguous).is_err());
        let refused = Expect::Refused {
            code: -22,
            message: "Block does not start with a coinbase",
        };
        let e = RpcError {
            code: Some(-22),
            message: "Block does not start with a coinbase".into(),
        };
        assert_eq!(self_check(&refused, &Err(e.clone()), &None), Ok(()));
        assert!(
            self_check(
                &refused,
                &Err(RpcError {
                    code: Some(-25),
                    ..e
                }),
                &None
            )
            .is_err()
        );
        assert_eq!(self_check(&Expect::Accept, &Ok(Value::Null), &None), Ok(()));
        assert!(self_check(&Expect::Accept, &Ok(Value::from("bad-cb-amount")), &None).is_err());
    }

    fn spend(prevout: (Hash, u32)) -> TxPlan {
        TxPlan::simple(
            vec![Input::new(prevout, Vec::new())],
            vec![(1_000, Lock::True.script_pubkey())],
        )
    }

    /// m-8: the trial's own transactions skip only a coinbase at `vtx[0]`.
    #[test]
    fn own_transactions_skip_only_the_coinbase() {
        let a = spend(([1; 32], 0));
        let b = spend(([2; 32], 0));
        let (with, ids) = plan_of(vec![coinbase(5, 1, 1), a.clone(), b.clone()]);
        assert_eq!(own_txids(&with, &ids), vec![a.txid(), b.txid()]);
        let (without, ids) = plan_of(vec![a.clone(), b.clone()]);
        assert_eq!(own_txids(&without, &ids), vec![a.txid(), b.txid()], "no coinbase to skip");
    }

    /// m-8, OD-L3-21: the rollback's delta gains only rolled-back transactions and loses only
    /// what the block took, each with its reason.
    #[test]
    fn the_rollback_mempool_delta() {
        let cold = spend(([1; 32], 0));
        let child = spend((cold.txid(), 0));
        let other = spend(([3; 32], 0));
        let mut seq = spend(([1; 32], 0));
        seq.outputs[0].0 = 999;
        let mut pool = Mempool::default();
        for tx in [&cold, &child, &other] {
            pool.txs.insert(tx.txid(), tx.clone());
        }
        let (plan, ids) = plan_of(vec![coinbase(5, 1, 1), seq.clone()]);
        let own = own_txids(&plan, &ids);
        let taken = pool.removed_for_block(&plan, &ids);
        let set = |txs: &[&TxPlan]| txs.iter().map(|t| hex(&t.txid())).collect::<BTreeSet<_>>();
        let before = set(&[&cold, &child, &other]);
        // Baseline: T-seq's own transaction back, T-cold's conflict and its child gone.
        let removed =
            mempool_delta(&before, &set(&[&seq, &other]), &own, &taken).expect("the rule");
        assert_eq!(
            removed,
            vec![
                (hex(&cold.txid()), format!("conflict on {}:0", hex(&[1; 32]))),
                (hex(&child.txid()), format!("descendant of {}", hex(&cold.txid()))),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
        // A gained transaction that was not rolled back.
        let stray = spend(([4; 32], 0));
        let e =
            mempool_delta(&before, &set(&[&seq, &other, &stray]), &own, &taken).expect_err("stray");
        assert!(e.contains("not a rolled-back"), "{e}");
        // A lost transaction the block did not take.
        let e = mempool_delta(&before, &set(&[&seq]), &own, &taken).expect_err("lost");
        assert!(e.contains(&hex(&other.txid())) && e.contains("neither confirms"), "{e}");
        // Nothing gained, nothing lost (ATMP declined the disconnected one).
        assert!(mempool_delta(&set(&[&other]), &set(&[&other]), &own, &BTreeMap::new()).is_ok());
    }

    fn delta(hook: u64, wrapped: u64) -> Delta {
        BTreeMap::from([("d.stx_hook", hook), ("w.stx", wrapped), ("g.stx", 0)])
    }

    /// M-3: the E-evo hook's per-body delta, and a dead hook failing.
    #[test]
    fn the_e_evo_hook_is_live_per_body() {
        let (setup, _) = plan_of(vec![coinbase(5, 1, 1)]);
        assert_eq!(liveness(&setup, Reach::Full, &delta(1, 1)), Ok(()));
        let dead = liveness(&setup, Reach::Full, &delta(0, 0)).expect_err("a dead hook");
        assert!(dead.contains("not live"), "{dead}");
        let (three, _) = plan_of(vec![coinbase(5, 1, 1), spend(([1; 32], 0)), spend(([2; 32], 0))]);
        assert_eq!(liveness(&three, Reach::Full, &delta(3, 3)), Ok(()), "N + 1");
        assert!(liveness(&three, Reach::Full, &delta(3, 2)).is_err());
        assert_eq!(liveness(&three, Reach::None, &delta(0, 0)), Ok(()));
        assert!(liveness(&three, Reach::None, &delta(3, 3)).is_err(), "counted a body never run");
        assert_eq!(liveness(&three, Reach::Partial, &delta(2, 2)), Ok(()));
        assert!(liveness(&three, Reach::Partial, &delta(0, 0)).is_err());
        // A raw type-5 coinbase is hooked, not wrapped (m-8's D2/D3 shape).
        let mut cb = coinbase(5, 1, 1);
        cb.tx_type = TRANSACTION_COINBASE;
        let (raw5, _) = plan_of(vec![cb, spend(([1; 32], 0))]);
        assert_eq!(liveness(&raw5, Reach::Full, &delta(2, 1)), Ok(()));
        assert!(liveness(&raw5, Reach::Full, &delta(2, 2)).is_err());
    }

    /// m-5: the plan's `g.mnhf` rule, with a10-5's OD-L3-18 baseline (a pre-DIP3 body with a
    /// signal, refused at st:228) passing.
    #[test]
    fn a_body_with_a_signal_is_refused_before_step_11() {
        let inv = Inventory::load().expect("the fixture");
        let bound = |key: &str| Some(Binding::Bound(key.into()));
        let st228 = bound("specialtxman.cpp:CheckSpecialTxInner#1:bad-tx-type-dip3-inactive");
        assert_eq!(mnhf_gate(&inv, &Ok(1), false, &st228), Ok(()), "the OD-L3-18 baseline");
        assert_eq!(mnhf_gate(&inv, &Ok(0), true, &None), Ok(()), "no signal");
        assert_eq!(mnhf_gate(&inv, &Err("bad".into()), false, &None), Ok(()), "recorded as such");
        assert!(mnhf_gate(&inv, &Ok(1), true, &None).is_err(), "accepted");
        let late = bound("validation.cpp:ConnectBlock#9:bad-cb-amount");
        let e = mnhf_gate(&inv, &Ok(1), false, &late).expect_err("after step 11");
        assert!(e.contains("evo/node.rs"), "names V's site: {e}");
        let ambiguous = Some(Binding::Ambiguous(vec!["x".into()]));
        assert!(mnhf_gate(&inv, &Ok(1), false, &ambiguous).is_err(), "not unique");
    }

    /// OD-L3-16's leg witness: a leg whose V load is refused stops before its first op with
    /// the load's status, and has no trial results.
    #[test]
    fn a_refused_v_load_stops_the_leg_before_op_0() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let inventory = Inventory::load().expect("the fixture");
        let dashd = Dashd {
            version: "24.0.0-rc.4".into(),
            binary: dir.path().join("never-started"),
            line: String::new(),
            sha256: String::new(),
            tag: inventory.provenance.tag.clone(),
            commit: inventory.provenance.commit.clone(),
        };
        let config = LegConfig {
            dashd: &dashd,
            par: Par::One,
            inventory: &inventory,
            only: None,
            record: false,
        };
        let mut args = chain_args(Par::One);
        args.push("-sporkkey=nonsense".into());
        let log = run_on(&config, dir.path(), args);
        assert_eq!(log.load_refused.as_deref(), Some("Unable to sign spork message, wrong key?"));
        assert_eq!(
            log.error.as_deref(),
            Some("V's load was refused: Unable to sign spork message, wrong key?")
        );
        assert!(log.ops.is_empty() && log.trials.is_empty() && log.dashd_argv.is_empty());
        // Its baseline: the same leg's V on the leg's own arguments serves.
        let v = NodeSide::with_argv(
            &chain_args(Par::One),
            T0,
            &private_dir(dir.path(), "ok").expect("a dir"),
            None,
        )
        .expect("a load");
        assert_eq!(v.down(), None);
    }

    /// rev5 §9: a funding read through the mempool overlay is refused.
    #[test]
    fn funding_reads_are_chain_only() {
        assert_eq!(planes::chain_only("gettxout", &json!(["00", 0, false])), Ok(()));
        assert!(planes::chain_only("gettxout", &json!(["00", 0, true])).is_err());
        assert!(
            planes::chain_only("gettxout", &json!(["00", 0])).is_err(),
            "Core's default is true"
        );
        assert_eq!(planes::chain_only("getblock", &json!(["00", 0])), Ok(()));
    }
}

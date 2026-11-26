//! O12b-2a's judge (PLAN-O12b-2a): reads one observation log and reports Stage-A criteria C1, C2
//! and C6 for a gate. C3, C4 and C5 are O12b-2c's and report [`Verdict::NotRun`] until it lands.
//!
//! The judged heights are `(h0, hN]`, from the first and last `Watermark`. A `Reorg` retracts the
//! height-keyed records above its fork height written before it, and the surviving `Block`s are
//! the reference chain. Core cites are to the v24.0.2 tag.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;

use serde::Serialize;

use super::contract::{
    Binding, Block, Chaos, Commitment, DigestCompare, DigestResult, Envelope, Hex32, Impl,
    LlmqParams, MnDiff, Record, RolePlan, Run, RunKind, SlotId, check_log,
};

pub const REHEARSAL_BANNER: &str = "REHEARSAL — not Stage-A evidence";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Gate {
    /// Passes iff the run is a Stage-A or soak run its plan admits and all six criteria pass.
    StageA,
    /// Passes iff the log is one well-formed run whose judged range is complete and linked.
    Rehearsal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "detail", rename_all = "kebab-case")]
pub enum Verdict {
    Pass,
    /// The first witness in height order.
    Fail(String),
    Vacuous(String),
    NotRun(String),
    /// The first missing piece of evidence.
    Incomplete(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Criterion {
    C1,
    C2,
    C3,
    C4,
    C5,
    C6,
}

impl Criterion {
    pub const ALL: [Criterion; 6] =
        [Criterion::C1, Criterion::C2, Criterion::C3, Criterion::C4, Criterion::C5, Criterion::C6];
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CriterionReport {
    pub criterion: Criterion,
    pub verdict: Verdict,
    pub n_applicable: usize,
    /// Applicable instances judged: C1's (opportunity, Rust member) pairs and null-only
    /// opportunities, C2's proTxs and C6's nodes. Exempt C1 pairs and C2 rows are listed in
    /// `exemptions`.
    pub n_checked: usize,
    pub exemptions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Report {
    pub gate: Gate,
    pub passed: bool,
    pub banner: Option<&'static str>,
    /// C1 to C6, in order.
    pub criteria: Vec<CriterionReport>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Heights a chaos window stays exempt after it ends. `None` is one `dkg_interval` of the
    /// instance's type, and for C2 the largest enabled one.
    pub chaos_allow: Option<u32>,
    /// Heights below the last watermark a node's digest compare may stop (C6).
    pub catchup: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            chaos_allow: None,
            catchup: 6,
        }
    }
}

/// Judges `log` for `gate`. A log that fails [`check_log`] is `Incomplete` everywhere.
pub fn judge(log: &[Envelope], gate: Gate, config: &Config) -> Report {
    let view = check_log(log).map_err(|e| e.to_string()).and_then(|()| View::new(log));
    let (criteria, passed) = match &view {
        Err(why) => (everywhere(&Verdict::Incomplete(why.clone())), false),
        Ok(view) => {
            let criteria = view.criteria(config);
            let kind = view.run.run_kind;
            let passed = match gate {
                Gate::Rehearsal => view.range.is_ok(),
                Gate::StageA => {
                    matches!(kind, RunKind::StageA | RunKind::Soak)
                        && view.plan.admits(kind)
                        && criteria.iter().all(|c| c.verdict == Verdict::Pass)
                }
            };
            (criteria, passed)
        }
    };
    Report {
        gate,
        passed,
        banner: (gate == Gate::Rehearsal).then_some(REHEARSAL_BANNER),
        criteria,
    }
}

fn bare(criterion: Criterion, verdict: Verdict) -> CriterionReport {
    CriterionReport {
        criterion,
        verdict,
        n_applicable: 0,
        n_checked: 0,
        exemptions: Vec::new(),
    }
}

fn everywhere(verdict: &Verdict) -> Vec<CriterionReport> {
    Criterion::ALL.into_iter().map(|c| bare(c, verdict.clone())).collect()
}

#[derive(Default)]
struct Tally {
    applicable: usize,
    checked: usize,
    fail: Option<(u32, String)>,
    incomplete: Option<String>,
    not_run: Option<String>,
    exemptions: Vec<String>,
}

impl Tally {
    fn fail(&mut self, height: u32, witness: String) {
        if self.fail.as_ref().is_none_or(|(first, _)| height < *first) {
            self.fail = Some((height, witness));
        }
    }

    fn incomplete(&mut self, missing: String) {
        self.incomplete.get_or_insert(missing);
    }

    fn report(self, criterion: Criterion) -> CriterionReport {
        let verdict = if let Some((_, witness)) = self.fail {
            Verdict::Fail(witness)
        } else if let Some(missing) = self.incomplete {
            Verdict::Incomplete(missing)
        } else if let Some(reason) = self.not_run {
            Verdict::NotRun(reason)
        } else if self.applicable == 0 {
            Verdict::Vacuous("no applicable instance".into())
        } else if self.checked == 0 {
            Verdict::Vacuous("every applicable instance is exempt".into())
        } else {
            Verdict::Pass
        };
        CriterionReport {
            criterion,
            verdict,
            n_applicable: self.applicable,
            n_checked: self.checked,
            exemptions: self.exemptions,
        }
    }
}

/// A chaos record with the heights at its start and end seqs.
type ChaosWindow<'a> = (&'a Chaos, u32, u32);

/// The surviving records of a log that passed [`check_log`].
struct View<'a> {
    run: &'a Run,
    plan: RolePlan,
    range: Result<(u32, u32), String>,
    /// The surviving block at each height.
    chain: BTreeMap<u32, &'a Block>,
    bindings: Vec<&'a Binding>,
    params: Result<Vec<&'a LlmqParams>, String>,
    commitments: Vec<&'a Commitment>,
    diffs: Vec<&'a MnDiff>,
    digests: Vec<&'a DigestCompare>,
    /// The chaos windows, or why one is unusable.
    chaos: Result<Vec<ChaosWindow<'a>>, String>,
}

impl<'a> View<'a> {
    fn new(log: &'a [Envelope]) -> Result<Self, String> {
        let Some(Record::Run(run)) = log.first().map(|e| &e.record) else {
            return Err("record 0 is not a Run".into());
        };
        let plan =
            RolePlan::new(run.preset, run.caps.clone(), run.context).map_err(|e| e.to_string())?;
        let reorgs: Vec<(u64, u32)> = log
            .iter()
            .filter_map(|e| match &e.record {
                Record::Reorg(r) => Some((e.seq, r.fork_height)),
                _ => None,
            })
            .collect();
        let survives = |seq: u64, height: u32| {
            !reorgs.iter().any(|&(reorg, fork)| seq < reorg && height > fork)
        };
        let mut chain: BTreeMap<u32, &Block> = BTreeMap::new();
        let mut forked = BTreeSet::new();
        let mut watermarks = Vec::new();
        let mut blocks = Vec::new();
        let mut chaos = Vec::new();
        let mut bindings = Vec::new();
        let mut params = Ok(Vec::new());
        let mut commitments = Vec::new();
        let mut diffs = Vec::new();
        let mut digests = Vec::new();
        for e in log {
            match &e.record {
                Record::Block(b) if survives(e.seq, b.height) => {
                    blocks.push((e.seq, b.height));
                    if let Some(old) = chain.insert(b.height, b)
                        && old.hash != b.hash
                    {
                        forked.insert(b.height);
                    }
                }
                Record::Commitment(c) if survives(e.seq, c.mined_height) => commitments.push(c),
                Record::MnDiff(d) if survives(e.seq, d.height) => diffs.push(d),
                Record::Watermark(w) => watermarks.push(w.height),
                Record::Binding(b) => bindings.push(b),
                Record::LlmqParams(p) => params = add_params(params, p, &watermarks),
                Record::DigestCompare(d) => digests.push(d),
                Record::Chaos(c) => chaos.push(c),
                _ => {}
            }
        }
        let last_seq = log.last().map_or(0, |e| e.seq);
        let height_at = |seq: u64| {
            blocks.iter().filter(|&&(s, _)| s <= seq).map(|&(_, h)| h).max().unwrap_or(0)
        };
        let chaos = chaos
            .into_iter()
            .map(|c| {
                if c.start_seq <= c.end_seq && c.end_seq <= last_seq {
                    Ok((c, height_at(c.start_seq), height_at(c.end_seq)))
                } else {
                    Err(format!("chaos {} is not a window of the log", c.id))
                }
            })
            .collect();
        Ok(View {
            run,
            plan,
            range: reference_range(&watermarks, &chain, &forked),
            chain,
            bindings,
            params: params.and_then(|p| {
                if p.is_empty() {
                    Err("no LlmqParams record".into())
                } else {
                    Ok(p)
                }
            }),
            commitments,
            diffs,
            digests,
            chaos,
        })
    }

    fn criteria(&self, config: &Config) -> Vec<CriterionReport> {
        if self.run.run_kind.is_rehearsal() || self.plan.run_kinds.iter().all(|k| k.is_rehearsal())
        {
            return everywhere(&Verdict::Vacuous("rehearsal".into()));
        }
        let range = match &self.range {
            Ok(range) => *range,
            Err(why) => return everywhere(&Verdict::Incomplete(why.clone())),
        };
        let twin = self.plan.slots.iter().any(|s| s.twin);
        let core_twin = |criterion| bare(criterion, Verdict::Vacuous("CoreTwin".into()));
        let later = |criterion| bare(criterion, Verdict::NotRun("O12b-2c".into()));
        let unusable = self.binding_error(range.0).or_else(|| self.chaos.as_ref().err().cloned());
        let rust = |criterion, judge: &dyn Fn(&[ChaosWindow]) -> CriterionReport| match (
            &self.chaos,
            &unusable,
        ) {
            _ if twin => core_twin(criterion),
            (Ok(chaos), None) => judge(chaos),
            (_, why) => bare(criterion, Verdict::Incomplete(why.clone().unwrap_or_default())),
        };
        vec![
            rust(Criterion::C1, &|chaos| self.c1(range, chaos, config)),
            rust(Criterion::C2, &|chaos| self.c2(range, chaos, config)),
            later(Criterion::C3),
            if twin {
                core_twin(Criterion::C4)
            } else {
                later(Criterion::C4)
            },
            later(Criterion::C5),
            self.c6(range, config),
        ]
    }

    /// Each binding is its slot's plan role, run by the run's executable for that role, and each
    /// Rust masternode slot is bound from the start of the range.
    fn binding_error(&self, h0: u32) -> Option<String> {
        let wrong = self.bindings.iter().find_map(|b| {
            let (exe, name) = match b.runs {
                Impl::Rust => (self.run.binaries.rdashd.as_ref(), "rdashd"),
                Impl::Dashd => (Some(&self.run.binaries.dashd), "dashd"),
            };
            if !self.plan.slot(b.slot).is_some_and(|s| s.label == b.label && s.runs == b.runs) {
                Some(format!("binding {} for {} is not the plan's role", b.pro_tx, b.slot))
            } else if exe.is_none_or(|exe| exe.sha256 != b.exe_sha256) {
                Some(format!("binding exe is not the run's {name}"))
            } else {
                None
            }
        });
        wrong.or_else(|| {
            self.plan
                .slots
                .iter()
                .filter(|s| s.runs == Impl::Rust && s.slot.is_masternode())
                .find(|s| {
                    !self.bindings.iter().any(|b| b.slot == s.slot && b.from_height <= h0 + 1)
                })
                .map(|s| format!("no binding for {} at {}", s.slot, h0 + 1))
        })
    }

    /// The slot whose binding at `height` is `pro_tx`, run by rdashd: each slot's binding at a
    /// height is its latest one from at or below it.
    fn rust_slot(&self, pro_tx: &Hex32, height: u32) -> Option<SlotId> {
        let mut current: BTreeMap<SlotId, &Binding> = BTreeMap::new();
        for b in self.bindings.iter().copied().filter(|b| b.from_height <= height) {
            let key = |b: &Binding| (b.from_height, b.from_seq);
            if current.get(&b.slot).is_none_or(|c| key(b) >= key(c)) {
                current.insert(b.slot, b);
            }
        }
        current.into_values().find(|b| b.pro_tx == *pro_tx && b.runs == Impl::Rust).map(|b| b.slot)
    }

    /// C1: every DKG opportunity in range mines a final commitment on the reference chain
    /// (blockprocessor.cpp:159, :311-324), and its Rust-bound members are valid.
    fn c1(&self, (h0, hn): (u32, u32), chaos: &[ChaosWindow], config: &Config) -> CriterionReport {
        let mut t = Tally::default();
        let rust_mn = |evo_only: bool| {
            self.plan.slots.iter().any(|s| {
                s.runs == Impl::Rust
                    && s.slot.is_masternode()
                    && (!evo_only || matches!(s.slot, SlotId::Evo(_)))
            })
        };
        if !rust_mn(false) {
            return t.report(Criterion::C1);
        }
        let params = match &self.params {
            Ok(params) => params,
            Err(why) => return bare(Criterion::C1, Verdict::Incomplete(why.clone())),
        };
        for p in params.iter().filter(|p| rust_mn(p.evo_only)) {
            let indexes = if p.rotation {
                u32::from(p.signing_active_quorum_count)
            } else {
                1
            };
            let allow = config.chaos_allow.unwrap_or(p.dkg_interval);
            let mut cycle = h0 - h0 % p.dkg_interval;
            while let Some(window_end) =
                cycle.checked_add(p.mining_window_end).filter(|&end| end <= hn)
            {
                let window = cycle + p.mining_window_start..=window_end;
                for index in (0..indexes).filter(|&i| cycle + i > h0) {
                    let at = Opportunity {
                        p,
                        index,
                        base: cycle + index,
                        window: window.clone(),
                    };
                    self.c1_opportunity(&mut t, &at, chaos, allow);
                }
                let Some(next) = cycle.checked_add(p.dkg_interval) else {
                    break;
                };
                cycle = next;
            }
        }
        t.report(Criterion::C1)
    }

    fn c1_opportunity(&self, t: &mut Tally, o: &Opportunity, chaos: &[ChaosWindow], allow: u32) {
        let at = format!("type {} base {}", o.p.llmq_type, o.base);
        let quorum_hash = self.chain.get(&o.base).map(|b| &b.hash);
        let mined: Vec<&Commitment> = self
            .commitments
            .iter()
            .copied()
            .filter(|c| {
                c.llmq_type == o.p.llmq_type
                    && u32::from(c.index) == o.index
                    && c.base_height == o.base
                    && quorum_hash == Some(&c.quorum_hash)
                    && o.window.contains(&c.mined_height)
            })
            .collect();
        let Some(final_commitment) =
            mined.iter().filter(|c| !c.null).min_by_key(|c| c.mined_height)
        else {
            t.applicable += 1;
            let down = self.stopped_slots(o, chaos, allow);
            if mined.is_empty() {
                t.incomplete(format!("{at}: no commitment"));
            } else if down.len() > usize::from(o.p.size - o.p.min_size) {
                let slots: Vec<String> = down.iter().map(SlotId::to_string).collect();
                t.exemptions.push(format!("{at}: null: chaos on {}", slots.join(", ")));
            } else {
                t.checked += 1;
                t.fail(o.base, format!("{at}: null"));
            }
            return;
        };
        for member in &final_commitment.members {
            let Some(slot) = self.rust_slot(&member.pro_tx, o.base) else {
                continue;
            };
            t.applicable += 1;
            let who = format!("{at}: {} (slot {slot})", member.pro_tx);
            if let Some(c) = chaos_hit(chaos, slot, o.base, *o.window.end(), allow) {
                t.exemptions.push(format!("{who}: chaos {}", c.id));
                continue;
            }
            t.checked += 1;
            if !member.valid {
                t.fail(o.base, format!("{who} invalid"));
            }
        }
    }

    /// The masternode slots eligible for the opportunity's type that a chaos window covering it
    /// targets. More than `size - min_size` of them can leave too few valid members for a final
    /// commitment, so Core mines a null one.
    fn stopped_slots(&self, o: &Opportunity, chaos: &[ChaosWindow], allow: u32) -> Vec<SlotId> {
        self.plan
            .slots
            .iter()
            .map(|s| s.slot)
            .filter(|&s| s.is_masternode() && (!o.p.evo_only || matches!(s, SlotId::Evo(_))))
            .filter(|&s| chaos_hit(chaos, s, o.base, *o.window.end(), allow).is_some())
            .collect()
    }

    /// C2: no PoSe penalty rise on a Rust-bound proTx. Core only lowers a penalty by one a block
    /// (deterministicmns.cpp:317-332), so a row above the last known value is a rise, and a PoSe
    /// ban raises the penalty in the same block (:281-298). Bans without one (a revoke or an
    /// operator key change, specialtxman.cpp:560, :609, :669) are not PoSe. Its instances are
    /// the proTxs; exempt rows are listed.
    fn c2(&self, (h0, hn): (u32, u32), chaos: &[ChaosWindow], config: &Config) -> CriterionReport {
        let mut t = Tally::default();
        let allow = match (config.chaos_allow, &self.params) {
            (Some(allow), _) => allow,
            (None, Ok(params)) => params.iter().map(|p| p.dkg_interval).max().unwrap_or(0),
            (None, Err(_)) if chaos.is_empty() => 0,
            (None, Err(why)) => return bare(Criterion::C2, Verdict::Incomplete(why.clone())),
        };
        let pro_txs: BTreeSet<&Hex32> = self.bindings.iter().map(|b| &b.pro_tx).collect();
        for pro_tx in pro_txs {
            let first = std::iter::once(h0 + 1)
                .chain(
                    self.bindings
                        .iter()
                        .filter(|b| {
                            b.pro_tx == *pro_tx && b.from_height > h0 && b.from_height <= hn
                        })
                        .map(|b| b.from_height),
                )
                .filter(|&h| self.rust_slot(pro_tx, h).is_some())
                .min();
            let Some(first) = first else {
                continue;
            };
            t.applicable += 1;
            let mut rows: Vec<&MnDiff> = self
                .diffs
                .iter()
                .copied()
                .filter(|d| d.pro_tx == *pro_tx && d.height <= hn)
                .collect();
            rows.sort_by_key(|d| d.height);
            let opens = |h: u32| h < first || (h == first && first > h0 + 1);
            let baseline = rows.first().filter(|d| opens(d.height)).and_then(|d| {
                d.pose_ban_height?;
                d.pose_revived_height?;
                d.pose_penalty
            });
            let Some(mut penalty) = baseline else {
                t.incomplete(format!("no PoSe baseline for {pro_tx}"));
                continue;
            };
            t.checked += 1;
            for d in &rows[1..] {
                let rose = d.pose_penalty.filter(|&p| p > penalty);
                penalty = d.pose_penalty.unwrap_or(penalty);
                let Some(p) = rose.filter(|_| d.height > h0) else {
                    continue;
                };
                let Some(slot) = self.rust_slot(pro_tx, d.height) else {
                    continue;
                };
                let who = format!("{pro_tx} (slot {slot}) at {}", d.height);
                if let Some(c) = chaos_hit(chaos, slot, d.height, d.height, allow) {
                    t.exemptions.push(format!("{who}: chaos {}", c.id));
                } else {
                    t.fail(d.height, format!("{who}: penalty rose to {p}"));
                }
            }
        }
        t.report(Criterion::C2)
    }

    /// C6: every node's state digests equal node0's over `[h0 + 1, hN - catchup]`. Chaos never
    /// exempts it, and any mismatch a node's records report fails it.
    fn c6(&self, (h0, hn): (u32, u32), config: &Config) -> CriterionReport {
        let mut t = Tally::default();
        let (from, to) = (h0 + 1, hn.saturating_sub(config.catchup));
        for slot in self.plan.slots.iter().map(|s| s.slot).filter(|&s| s != SlotId::CONTROLLER) {
            t.applicable += 1;
            let found: Vec<&DigestCompare> =
                self.digests.iter().copied().filter(|d| d.node == slot).collect();
            let mismatch = found
                .iter()
                .filter_map(|d| match d.result {
                    DigestResult::Mismatch {
                        height,
                    } => Some(height),
                    _ => None,
                })
                .min();
            if let Some(height) = mismatch {
                t.checked += 1;
                t.fail(height, format!("{slot} differs from node0 at {height}"));
                continue;
            }
            let [d] = found[..] else {
                t.incomplete(match found.len() {
                    0 => format!("no digest compare for {slot}"),
                    n => format!("{slot} has {n} digest compares"),
                });
                continue;
            };
            match &d.result {
                _ if to < from => t.incomplete(format!(
                    "the judged range ends within the {} catch-up",
                    config.catchup
                )),
                _ if d.from_height > from || d.to_height < to => t.incomplete(format!(
                    "{slot} digest covers [{}, {}], not [{from}, {to}]",
                    d.from_height, d.to_height
                )),
                DigestResult::Equal
                | DigestResult::Mismatch {
                    ..
                } => t.checked += 1,
                DigestResult::NotRun {
                    reason,
                } => {
                    t.not_run.get_or_insert(format!("{slot}: {reason}"));
                }
                DigestResult::Incomplete {
                    reason,
                } => t.incomplete(format!("{slot}: {reason}")),
            }
        }
        t.report(Criterion::C6)
    }
}

struct Opportunity<'p> {
    p: &'p LlmqParams,
    index: u32,
    base: u32,
    window: RangeInclusive<u32>,
}

/// The chaos window that targets `slot` and whose cover, from the highest surviving block height
/// at its start seq to that at its end seq plus `allow`, overlaps `[lo, hi]`.
fn chaos_hit<'a>(
    chaos: &[ChaosWindow<'a>],
    slot: SlotId,
    lo: u32,
    hi: u32,
    allow: u32,
) -> Option<&'a Chaos> {
    chaos
        .iter()
        .find(|(c, start, end)| {
            c.targets.contains(&slot) && *start <= hi && lo <= end.saturating_add(allow)
        })
        .map(|(c, _, _)| *c)
}

/// `LlmqParams` come before any `Watermark`, once per type, with a DKG schedule Core could run:
/// the mining window inside the interval, after every rotation index's base height.
fn add_params<'a>(
    params: Result<Vec<&'a LlmqParams>, String>,
    p: &'a LlmqParams,
    watermarks: &[u32],
) -> Result<Vec<&'a LlmqParams>, String> {
    let mut params = params?;
    let t = p.llmq_type;
    let indexes = if p.rotation {
        u32::from(p.signing_active_quorum_count)
    } else {
        1
    };
    if !watermarks.is_empty() {
        Err(format!("LlmqParams type {t} after a Watermark"))
    } else if params.iter().any(|q| q.llmq_type == t) {
        Err(format!("LlmqParams type {t} twice"))
    } else if indexes == 0
        || indexes > p.mining_window_start
        || p.mining_window_start > p.mining_window_end
        || p.mining_window_end >= p.dkg_interval
    {
        Err(format!("LlmqParams type {t} has no DKG window"))
    } else if p.min_size == 0 || p.min_size > p.size {
        Err(format!("LlmqParams type {t} has no quorum size"))
    } else {
        params.push(p);
        Ok(params)
    }
}

/// `(h0, hN)` if at least two watermarks bound a range whose every height has one surviving block
/// linked to the one below it.
fn reference_range(
    watermarks: &[u32],
    chain: &BTreeMap<u32, &Block>,
    forked: &BTreeSet<u32>,
) -> Result<(u32, u32), String> {
    let (h0, hn) = match watermarks {
        [first, .., last] if last > first => (*first, *last),
        _ => return Err("no judged range".into()),
    };
    for h in h0 + 1..=hn {
        let Some(block) = chain.get(&h) else {
            return Err(format!("height {h} missing"));
        };
        if forked.contains(&h) {
            return Err(format!("height {h} has two blocks"));
        }
        if h - 1 > h0 && chain.get(&(h - 1)).is_some_and(|below| block.prev != below.hash) {
            return Err(format!("height {h} does not link to height {}", h - 1));
        }
    }
    Ok((h0, hn))
}

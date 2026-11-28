//! C3, C4 and C5 (PLAN-O12b-2c), judged from O12b-5's driver records.

use super::{
    ChaosWindow, Config, Criterion, CriterionReport, Tally, Verdict, View, bare, chaos_hit,
};
use crate::mixnet::contract::{
    Cap, Commitment, DkgCase, DkgCaseKind, Hex32, Impl, ProbeKind, ProbeRecovery, Proposal, SlotId,
    TipStatus,
};

/// The values C4 compares between a case's two halves. Member order differs between quorums, so
/// they are the ones that do not depend on the target's position.
const OUTCOME: [&str; 7] = [
    "null",
    "target_valid",
    "others_valid",
    "members",
    "complaints",
    "justifications",
    "pose_delta",
];

fn impl_name(runs: Impl) -> &'static str {
    match runs {
        Impl::Rust => "rust",
        Impl::Dashd => "dashd",
    }
}

impl View<'_> {
    /// The surviving commitment of `(llmq_type, quorum_hash)`, final before null, whose base block
    /// is the reference chain's where the chain has one.
    fn quorum(&self, llmq_type: u8, quorum_hash: &Hex32) -> Option<&Commitment> {
        self.commitments
            .iter()
            .copied()
            .filter(|c| {
                c.llmq_type == llmq_type
                    && c.quorum_hash == *quorum_hash
                    && self.chain.get(&c.base_height).is_none_or(|b| b.hash == c.quorum_hash)
            })
            .min_by_key(|c| (c.null, c.mined_height))
    }

    fn runs(&self, slot: SlotId) -> Option<Impl> {
        self.plan.slot(slot).map(|s| s.runs)
    }

    /// C3: for each enabled kind, the Rust members of an isolated quorum recover a fresh
    /// signature that node 0 verifies in time. Its instances are the enabled kinds.
    pub(super) fn c3(&self) -> CriterionReport {
        let mut t = Tally::default();
        if !self.rust_masternode(false) {
            return t.report(Criterion::C3);
        }
        for kind in ProbeKind::ALL.into_iter().filter(|k| self.run.caps.has(k.cap())) {
            t.applicable += 1;
            let mut rejected = None;
            let mut counted = Vec::new();
            for p in self.probes.iter().copied().filter(|p| p.probe == kind) {
                match self.probe_problem(p) {
                    Some(why) => {
                        rejected.get_or_insert(why);
                    }
                    None => counted.push(p),
                }
            }
            if counted.is_empty() {
                if let Some(skip) = self.skips.iter().find(|s| s.probe == kind) {
                    t.not_run.get_or_insert(format!("{}: {}", kind.name(), skip.reason));
                } else {
                    let why = rejected.map_or_else(String::new, |why| format!(" ({why})"));
                    t.incomplete(format!("no valid {} probe{why}", kind.name()));
                }
                continue;
            }
            t.checked += 1;
            for p in counted {
                let at = format!("{} {}", kind.name(), p.request_id);
                if !p.verified {
                    t.fail(0, format!("{at}: not verified"));
                } else if p.elapsed_ms > p.deadline_ms {
                    t.fail(0, format!("{at}: deadline missed"));
                }
            }
        }
        t.report(Criterion::C3)
    }

    /// Why a probe is not C3 evidence, if it is not.
    fn probe_problem(&self, p: &ProbeRecovery) -> Option<String> {
        let isolated = p.isolation_start_seq < p.requested_seq
            && p.requested_seq <= p.result_seq
            && p.result_seq <= p.isolation_end_seq;
        if !isolated {
            return Some("stale or not isolated throughout".into());
        }
        let running = self.plan.slots.iter().find(|s| {
            s.slot.is_masternode() && s.runs == Impl::Dashd && !p.stopped.contains(&s.slot)
        });
        if let Some(s) = running {
            return Some(format!("{} not stopped", s.slot));
        }
        if p.verified_quorum.as_ref() != Some(&p.quorum_hash) {
            return Some("wrong quorum".into());
        }
        let Some(params) = self.params.iter().flatten().find(|q| q.llmq_type == p.llmq_type) else {
            return Some(format!("no LlmqParams for type {}", p.llmq_type));
        };
        let Some(c) = self.quorum(p.llmq_type, &p.quorum_hash).filter(|c| !c.null) else {
            return Some("no final commitment".into());
        };
        let rust = c
            .members
            .iter()
            .filter(|m| m.valid && self.rust_slot(&m.pro_tx, c.base_height).is_some())
            .count();
        (rust < usize::from(params.threshold))
            .then(|| format!("{rust} Rust valid members, threshold {}", params.threshold))
    }

    /// C4: each case gives the same outcome with a Rust target as with a dashd target (PLAN-O12b
    /// R-b: equality only). Its instances are the cases.
    pub(super) fn c4(&self) -> CriterionReport {
        let mut t = Tally::default();
        if !self.run.caps.has(Cap::Dkg) || !self.rust_masternode(false) {
            return t.report(Criterion::C4);
        }
        for (order, case) in (0u32..).zip(DkgCaseKind::ALL) {
            t.applicable += 1;
            let half = |runs| self.case_outcome(case, runs);
            match (half(Impl::Rust), half(Impl::Dashd)) {
                (Ok(rust), Ok(dashd)) => {
                    t.checked += 1;
                    if let Some(i) = (0..OUTCOME.len()).find(|&i| rust[i] != dashd[i]) {
                        let (name, a, b) = (OUTCOME[i], rust[i], dashd[i]);
                        t.fail(order, format!("{}: {name} rust={a} dashd={b}", case.name()));
                    }
                }
                (Err(why), _) | (_, Err(why)) => t.incomplete(format!("{}: {why}", case.name())),
            }
        }
        t.report(Criterion::C4)
    }

    /// The outcome of `case`'s one run on a masternode slot running `runs`.
    fn case_outcome(&self, case: DkgCaseKind, runs: Impl) -> Result<[i64; 7], String> {
        let name = impl_name(runs);
        let found: Vec<&DkgCase> = self
            .dkg_cases
            .iter()
            .copied()
            .filter(|d| {
                d.case == case && d.target.is_masternode() && self.runs(d.target) == Some(runs)
            })
            .collect();
        let [d] = found[..] else {
            return Err(match found.len() {
                0 => format!("no {name} target"),
                n => format!("{n} {name} targets"),
            });
        };
        let Some(c) = self.quorum(d.llmq_type, &d.quorum_hash) else {
            return Err(format!("no commitment for the {name} target's quorum"));
        };
        let pro_tx = self.bindings_at(c.base_height).get(&d.target).map(|b| &b.pro_tx);
        let target = c.members.iter().position(|m| Some(&m.pro_tx) == pro_tx);
        let (target_valid, others_valid) = match target {
            Some(i) => {
                let others = c.members.iter().enumerate().filter(|&(j, m)| j != i && m.valid);
                (c.members[i].valid, others.count())
            }
            None if c.null => (false, 0),
            None => return Err(format!("the {name} target is not a member")),
        };
        let count = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
        Ok([
            i64::from(c.null),
            i64::from(target_valid && !c.null),
            count(others_valid),
            count(c.members.len()),
            i64::from(d.complaints),
            i64::from(d.justifications),
            d.pose_delta,
        ])
    }

    /// C5: every opposite-impl slot accepts each producer impl's blocks (mining.cpp:697-724).
    /// Its instances are (block, validator) proposals.
    pub(super) fn c5(&self, chaos: &[ChaosWindow], config: &Config) -> CriterionReport {
        let mut t = Tally::default();
        let has = |runs| self.plan.slots.iter().any(|s| s.runs == runs);
        if !self.run.caps.has(Cap::Gbt) || !has(Impl::Rust) || !has(Impl::Dashd) {
            return t.report(Criterion::C5);
        }
        let allow = match self.height_allow(chaos, config) {
            Ok(allow) => allow,
            Err(why) => return bare(Criterion::C5, Verdict::Incomplete(why)),
        };
        for (producer, validator) in [(Impl::Rust, Impl::Dashd), (Impl::Dashd, Impl::Rust)] {
            let dir = format!("{} -> {}", impl_name(producer), impl_name(validator));
            let proposals: Vec<&Proposal> = self
                .proposals
                .iter()
                .copied()
                .filter(|p| {
                    self.runs(p.producer) == Some(producer)
                        && self.runs(p.validator) == Some(validator)
                })
                .collect();
            if proposals.is_empty() {
                t.applicable += 1;
                t.incomplete(format!("{dir}: no block"));
                continue;
            }
            let unchecked =
                self.plan.slots.iter().find(|s| {
                    s.runs == validator && proposals.iter().all(|p| p.validator != s.slot)
                });
            if let Some(s) = unchecked {
                t.incomplete(format!("{dir}: no proposal to {}", s.slot));
            }
            for p in proposals {
                t.applicable += 1;
                let who = format!("{dir}: block {} at {} on {}", p.block, p.height, p.validator);
                let record = match self.miners.get(&p.block) {
                    None => Some("no block record".to_owned()),
                    Some(&(height, _)) if height != p.height => {
                        Some(format!("its block record is at {height}"))
                    }
                    Some(&(_, miner)) if miner != Some(p.producer) => {
                        Some("its block record names another miner".to_owned())
                    }
                    Some(_) => None,
                };
                if let Some(why) = record {
                    t.incomplete(format!("{who}: {why}"));
                    continue;
                }
                let hit = [p.validator, p.producer]
                    .into_iter()
                    .find_map(|slot| chaos_hit(chaos, slot, p.height, p.height, allow));
                if let Some(c) = hit {
                    t.exemptions.push(format!("{who}: chaos {}", c.id));
                    continue;
                }
                match validation(p) {
                    Validation::Accept => t.checked += 1,
                    Validation::Fail(why) => {
                        t.checked += 1;
                        t.fail(p.height, format!("{who}: {why}"));
                    }
                    Validation::Inconclusive(why) => t.incomplete(format!("{who}: {why}")),
                }
            }
        }
        t.report(Criterion::C5)
    }
}

enum Validation {
    Accept,
    Fail(String),
    Inconclusive(String),
}

/// Accept: the proposal is null or `duplicate` and the block is active or a valid fork. Fail:
/// `duplicate-invalid`, a reject reason (mining.cpp:532-547) or an invalid tip. Otherwise the
/// result is inconclusive (mining.cpp:714, :720) or the tip is not settled.
fn validation(p: &Proposal) -> Validation {
    if p.tip == Some(TipStatus::Invalid) {
        return Validation::Fail("invalid tip".into());
    }
    match p.result.as_deref() {
        Some(r) if r == "duplicate-inconclusive" || r.starts_with("inconclusive") => {
            Validation::Inconclusive(r.into())
        }
        Some(r) if r != "duplicate" => Validation::Fail(format!("rejected: {r}")),
        _ if matches!(p.tip, Some(TipStatus::Active | TipStatus::ValidFork)) => Validation::Accept,
        _ => Validation::Inconclusive(format!("tip {:?}", p.tip)),
    }
}

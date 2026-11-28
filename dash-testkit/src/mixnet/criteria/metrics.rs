//! O12b-2b (PLAN-O12b-2b): rates and latencies over a log's judged range, and the divergences
//! that page the owner. Every rate carries its denominator.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{ChaosWindow, Config, View, chaos_hit, final_of, opportunities};
use crate::mixnet::contract::{DigestResult, Envelope, Hex32, Impl, Record, SlotId, check_log};

/// An InstantSend lock seen later than this after its request is a failure.
const IS_TIMEOUT_NS: u64 = 60_000_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Ratio {
    pub num: u64,
    pub den: u64,
}

impl Ratio {
    fn add(&mut self, hit: bool) {
        self.den += 1;
        self.num += u64::from(hit);
    }
}

/// Nearest-rank percentiles of a set of samples, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Latency {
    pub n: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
}

impl Latency {
    fn of(mut ns: Vec<u64>) -> Self {
        ns.sort_unstable();
        let n = ns.len();
        let rank = |pct: usize| {
            ns.get((n * pct).div_ceil(100).saturating_sub(1)).map_or(0, |v| v / 1_000_000)
        };
        Latency {
            n: n as u64,
            p50_ms: rank(50),
            p95_ms: rank(95),
            max_ms: rank(100),
        }
    }
}

/// Who runs a proTx: the `runs` of its slot's binding, or no binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    Rust,
    Dashd,
    Unbound,
}

impl From<Impl> for Owner {
    fn from(runs: Impl) -> Self {
        match runs {
            Impl::Rust => Owner::Rust,
            Impl::Dashd => Owner::Dashd,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OwnerRate {
    pub owner: Owner,
    pub rate: Ratio,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OwnerCount {
    pub owner: Owner,
    pub n: u64,
}

/// One LLMQ type's DKGs: opportunities that mined a final commitment, and per impl, those whose
/// final commitment has a valid member of it among opportunities that impl could join. An impl
/// whose slots a smaller quorum did not select counts as not succeeding, for either impl alike.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DkgRate {
    pub llmq_type: u8,
    #[serde(rename = "final")]
    pub final_: Ratio,
    pub by_impl: Vec<OwnerRate>,
}

/// ChainLocks on node 0: latency from the block's record, and heights locked by name. Locks of
/// the last heights can land after the log ends, so a clean run can cover slightly under 1.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ClMetrics {
    pub latency: Latency,
    pub coverage: Ratio,
    /// Locks recorded before their block, which give no latency sample.
    pub unordered: u64,
}

/// InstantSend locks on node 0: seen within the timeout over requests decided either way.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct IsMetrics {
    pub latency: Latency,
    pub success: Ratio,
    /// Requests neither seen nor timed out when the log ends.
    pub pending: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CjCount {
    pub owner: Owner,
    pub outcome: String,
    pub n: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PageKind {
    DigestMismatch,
    BestHash,
    ClConflict,
    Pose,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Page {
    pub kind: PageKind,
    pub height: u32,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Metrics {
    pub range: (u32, u32),
    pub dkg: Vec<DkgRate>,
    /// Valid members of final commitments bound to a Rust slot, over all valid members.
    pub rust_share: Ratio,
    pub chainlocks: ClMetrics,
    pub instantsend: IsMetrics,
    pub pose: Vec<OwnerCount>,
    pub coinjoin: Vec<CjCount>,
    pub superblocks: Vec<OwnerRate>,
    /// One per (kind, height), by height then kind.
    pub pages: Vec<Page>,
    /// PoSe events on Rust-bound proTxs that a chaos window exempts from paging.
    pub exempt: Vec<String>,
}

/// O12b-2b's metrics and pages for `log`. `Err` is a `check_log`, `View`, range or
/// `LlmqParams` error.
pub fn metrics(log: &[Envelope], config: &Config) -> Result<Metrics, String> {
    check_log(log).map_err(|e| e.to_string())?;
    let view = View::new(log)?;
    let range = view.range.clone()?;
    view.metrics(log, range, config)
}

/// A PoSe penalty rise on a proTx.
struct PoseEvent<'a> {
    pro_tx: &'a Hex32,
    height: u32,
    penalty: u32,
}

impl<'a> View<'a> {
    fn owner(&self, pro_tx: &Hex32, height: u32) -> Owner {
        self.bindings_at(height)
            .into_values()
            .find(|b| b.pro_tx == *pro_tx)
            .map_or(Owner::Unbound, |b| b.runs.into())
    }

    fn metrics(
        &self,
        log: &'a [Envelope],
        range: (u32, u32),
        config: &Config,
    ) -> Result<Metrics, String> {
        let (dkg, rust_share) = self.dkg(range)?;
        let chaos: &[ChaosWindow] = self.chaos.as_deref().unwrap_or_default();
        let allow = self.height_allow(chaos, config).unwrap_or(0);
        let mut pages = BTreeMap::new();
        let mut exempt = Vec::new();
        let mut pose = BTreeMap::new();
        let mut paged = BTreeSet::new();
        for e in self.pose_events(range) {
            let owner = self.owner(e.pro_tx, e.height);
            *pose.entry(owner).or_insert(0) += 1;
            let Some(slot) = self.rust_slot(e.pro_tx, e.height) else {
                continue;
            };
            let who = format!(
                "{} (slot {slot}) at {}: penalty rose to {}",
                e.pro_tx, e.height, e.penalty
            );
            if let Some(c) = chaos_hit(chaos, slot, e.height, e.height, allow) {
                exempt.push(format!("{who}: chaos {}", c.id));
            } else if paged.insert(e.pro_tx) {
                page(&mut pages, e.height, PageKind::Pose, who);
            }
        }
        self.divergences(log, range, config, &mut pages);
        Ok(Metrics {
            range,
            dkg,
            rust_share,
            chainlocks: self.chainlocks(log, range),
            instantsend: instantsend(log),
            pose: pose
                .into_iter()
                .map(|(owner, n)| OwnerCount {
                    owner,
                    n,
                })
                .collect(),
            coinjoin: self.coinjoin(log),
            superblocks: self.superblocks(range),
            pages: pages
                .into_iter()
                .map(|((height, kind), detail)| Page {
                    kind,
                    height,
                    detail,
                })
                .collect(),
            exempt,
        })
    }

    fn dkg(&self, range: (u32, u32)) -> Result<(Vec<DkgRate>, Ratio), String> {
        let params = self.params.as_ref().map_err(String::clone)?;
        let mut rust_share = Ratio::default();
        let mut rates = Vec::new();
        for p in params.iter().copied() {
            let mut final_ = Ratio::default();
            let mut by_impl: BTreeMap<Owner, Ratio> = BTreeMap::new();
            let eligible: BTreeSet<Owner> = self
                .plan
                .slots
                .iter()
                .filter(|s| {
                    s.slot.is_masternode() && (!p.evo_only || matches!(s.slot, SlotId::Evo(_)))
                })
                .map(|s| s.runs.into())
                .collect();
            for o in opportunities(p, range) {
                let mined = self.mined(&o);
                let commitment = final_of(&mined);
                final_.add(commitment.is_some());
                let valid: Vec<Owner> = commitment
                    .iter()
                    .flat_map(|c| &c.members)
                    .filter(|m| m.valid)
                    .map(|m| self.owner(&m.pro_tx, o.base))
                    .collect();
                for v in &valid {
                    rust_share.add(*v == Owner::Rust);
                }
                for owner in &eligible {
                    by_impl.entry(*owner).or_default().add(valid.contains(owner));
                }
            }
            rates.push(DkgRate {
                llmq_type: p.llmq_type,
                final_,
                by_impl: by_impl
                    .into_iter()
                    .map(|(owner, rate)| OwnerRate {
                        owner,
                        rate,
                    })
                    .collect(),
            });
        }
        Ok((rates, rust_share))
    }

    /// Node 0's first lock of each in-range height's surviving block, timed from that block's
    /// first record.
    fn chainlocks(&self, log: &[Envelope], (h0, hn): (u32, u32)) -> ClMetrics {
        let on_chain = |height: u32, hash: &Hex32| {
            height > h0 && height <= hn && self.chain.get(&height).is_some_and(|b| b.hash == *hash)
        };
        let mut block_at: BTreeMap<u32, &Envelope> = BTreeMap::new();
        let mut locked_at: BTreeMap<u32, &Envelope> = BTreeMap::new();
        for e in log {
            match &e.record {
                Record::Block(b) if on_chain(b.height, &b.hash) => {
                    block_at.entry(b.height).or_insert(e);
                }
                Record::ClSeen(c)
                    if c.node == SlotId::CONTROLLER && on_chain(c.height, &c.hash) =>
                {
                    locked_at.entry(c.height).or_insert(e);
                }
                _ => {}
            }
        }
        let mut samples = Vec::new();
        let mut unordered = 0;
        for (height, lock) in &locked_at {
            match block_at.get(height) {
                Some(block) if block.seq < lock.seq => {
                    samples.push(lock.mono_ns.saturating_sub(block.mono_ns));
                }
                _ => unordered += 1,
            }
        }
        ClMetrics {
            latency: Latency::of(samples),
            coverage: Ratio {
                num: locked_at.len() as u64,
                den: u64::from(hn - h0),
            },
            unordered,
        }
    }

    /// PoSe penalty rises in range: a row above the proTx's previous known penalty. A proTx's
    /// first row has nothing to compare with.
    fn pose_events(&self, (h0, hn): (u32, u32)) -> Vec<PoseEvent<'a>> {
        let mut rows = self.diffs.clone();
        rows.sort_by_key(|d| (&d.pro_tx, d.height));
        let mut last: BTreeMap<&Hex32, u32> = BTreeMap::new();
        let mut events = Vec::new();
        for d in rows {
            let Some(penalty) = d.pose_penalty else {
                continue;
            };
            let before = last.insert(&d.pro_tx, penalty);
            if before.is_some_and(|b| penalty > b) && d.height > h0 && d.height <= hn {
                events.push(PoseEvent {
                    pro_tx: &d.pro_tx,
                    height: d.height,
                    penalty,
                });
            }
        }
        events.sort_by_key(|e| (e.height, e.pro_tx));
        events
    }

    fn coinjoin(&self, log: &[Envelope]) -> Vec<CjCount> {
        let mut counts: BTreeMap<(Owner, &str), u64> = BTreeMap::new();
        for e in log {
            let Record::CjSession(s) = &e.record else {
                continue;
            };
            let owner = self
                .bindings
                .iter()
                .filter(|b| b.pro_tx == s.coordinator_pro_tx && b.from_seq <= e.seq)
                .max_by_key(|b| b.from_seq)
                .map_or(Owner::Unbound, |b| b.runs.into());
            *counts.entry((owner, &s.outcome)).or_default() += 1;
        }
        counts
            .into_iter()
            .map(|((owner, outcome), n)| CjCount {
                owner,
                outcome: outcome.into(),
                n,
            })
            .collect()
    }

    fn superblocks(&self, (h0, hn): (u32, u32)) -> Vec<OwnerRate> {
        let mut rates: BTreeMap<Owner, Ratio> = BTreeMap::new();
        for b in self.superblocks.iter().filter(|b| b.height > h0 && b.height <= hn) {
            rates.entry(self.owner(&b.creator_pro_tx, b.height)).or_default().add(b.paid);
        }
        rates
            .into_iter()
            .map(|(owner, rate)| OwnerRate {
                owner,
                rate,
            })
            .collect()
    }

    fn divergences(
        &self,
        log: &[Envelope],
        (h0, hn): (u32, u32),
        config: &Config,
        pages: &mut BTreeMap<(u32, PageKind), String>,
    ) {
        for d in &self.digests {
            if let DigestResult::Mismatch {
                height,
            } = d.result
            {
                page(pages, height, PageKind::DigestMismatch, d.node.to_string());
            }
        }
        let mut recorded = self.recorded.iter().peekable();
        let mut reference = 0;
        let mut locks: BTreeMap<u32, BTreeSet<&Hex32>> = BTreeMap::new();
        for e in log {
            while let Some(&(_, height)) = recorded.next_if(|&&(seq, _)| seq <= e.seq) {
                reference = reference.max(height);
            }
            match &e.record {
                Record::Tip(t) if t.height > h0 && t.height <= hn => {
                    let Some(block) = self.chain.get(&t.height) else {
                        continue;
                    };
                    let settled = reference >= t.height.saturating_add(config.catchup);
                    if block.hash != t.hash && settled {
                        let detail = format!(
                            "{} at {}: {}, reference {}",
                            t.node, t.height, t.hash, block.hash
                        );
                        page(pages, t.height, PageKind::BestHash, detail);
                    }
                }
                Record::ClSeen(c) => {
                    locks.entry(c.height).or_default().insert(&c.hash);
                }
                _ => {}
            }
        }
        for (height, hashes) in locks.into_iter().filter(|(_, h)| h.len() > 1) {
            let names: Vec<String> = hashes.iter().map(|h| h.to_string()).collect();
            pages.insert((height, PageKind::ClConflict), names.join(", "));
        }
    }
}

/// Node 0's first lock of each requested txid, timed from its first request. A lock recorded
/// before its request (the collector merges two sources) takes no time.
fn instantsend(log: &[Envelope]) -> IsMetrics {
    let end = log.iter().map(|e| e.mono_ns).max().unwrap_or(0);
    let mut requests: BTreeMap<&Hex32, &Envelope> = BTreeMap::new();
    let mut seen: BTreeMap<&Hex32, &Envelope> = BTreeMap::new();
    for e in log {
        match &e.record {
            Record::IsReq(r) => {
                requests.entry(&r.txid).or_insert(e);
            }
            Record::IsSeen(s) if s.node == SlotId::CONTROLLER => {
                seen.entry(&s.txid).or_insert(e);
            }
            _ => {}
        }
    }
    let mut m = IsMetrics::default();
    let mut samples = Vec::new();
    for (txid, req) in requests {
        match seen.get(txid) {
            Some(lock) => {
                let took = lock.mono_ns.saturating_sub(req.mono_ns);
                samples.push(took);
                m.success.add(took <= IS_TIMEOUT_NS);
            }
            None if end.saturating_sub(req.mono_ns) >= IS_TIMEOUT_NS => m.success.add(false),
            None => m.pending += 1,
        }
    }
    m.latency = Latency::of(samples);
    m
}

/// Adds `detail` to the one page of `(height, kind)`.
fn page(
    pages: &mut BTreeMap<(u32, PageKind), String>,
    height: u32,
    kind: PageKind,
    detail: String,
) {
    match pages.get_mut(&(height, kind)) {
        None => {
            pages.insert((height, kind), detail);
        }
        Some(found) if !found.split("; ").any(|d| d == detail) => {
            found.push_str("; ");
            found.push_str(&detail);
        }
        Some(_) => {}
    }
}

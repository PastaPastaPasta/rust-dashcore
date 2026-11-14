//! Incremental StateDigest comparison. Live RPC, status-line rendering and hooks attach here.

pub mod alarm;
pub mod settle;
pub mod side;
pub mod state;
pub mod tail;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, ensure};
use dash_statedigest::compare::{FieldId, Mode, Options, Report, Verdict, What, compare_linear};
use dash_statedigest::{
    Class as FieldClass, FieldSource, Hash256, Kind, Origin, Outcome, Record, Value, group,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::digest::compare::Input;
use crate::digest::dump::{self, Extras, RpcTarget};
use crate::digest::extract::{Client, NodeRpc, golden, set_golden};
use crate::digest::store::Records;
use alarm::{Alarm, Class, Crit, Warn};
use settle::{ChainLock, Settle};
use side::Side;
use state::{Checkpoint, Settled, State, Status};
use tail::Tail;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerStatus {
    pub index_height: i64,
    pub headers: i64,
    pub chainlock: Option<ChainLock>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeStatus {
    pub warnings: String,
    pub chainlock: Option<ChainLock>,
}

pub enum Enrichment {
    Ready(Outcome),
    Reorg,
}

pub trait PeerRpc {
    fn status(&self) -> Result<PeerStatus>;
    fn node(&self) -> Result<&dyn NodeRpc>;

    fn enrich(&self, record: &Outcome) -> Result<Enrichment> {
        let node = self.node()?;
        let info = node
            .call("gettxoutsetinfo", json!(["muhash", record.id.h, true]))?
            .map_err(|e| anyhow!("gettxoutsetinfo: {}: {}", e.code, e.message))?;
        if info.get(b"bestblock").and_then(Value::as_hash) != Some(record.id.b) {
            return Ok(Enrichment::Reorg);
        }
        let mut record = record.clone();
        let values = golden(
            &Client {
                node,
            },
            record.id.h,
            &record.id.b,
        )?;
        set_golden(&mut record, &values)?;
        Ok(Enrichment::Ready(record))
    }
}

pub trait RdashdRpc {
    fn status(&self) -> Result<NodeStatus>;
}

pub struct Config {
    pub left: PathBuf,
    pub right: PathBuf,
    pub out: PathBuf,
    /// Exact warning text supplied by the tripwire's live adapter.
    pub tripwire: String,
}

pub struct Shadow {
    pub sides: [Side; 2],
    pub state: State,
    out: PathBuf,
    tripwire: String,
    pub mismatch: Option<Report>,
    #[allow(dead_code)]
    lock: File,
}

impl Shadow {
    pub fn open(config: Config, now: u64) -> Result<Self> {
        ensure!(!config.tripwire.is_empty(), "tripwire text must not be empty");
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&config.out)?;
        fs::set_permissions(&config.out, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(config.out.join(".lock"))?;
        let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        ensure!(rc == 0, "shadow state directory is already locked by another writer");
        let saved = State::read(&config.out.join("state.json"))?;
        let paths = [config.left, config.right];
        let sides = if let Some(state) = &saved {
            ensure!(
                state.sides[0].tail.path == paths[0] && state.sides[1].tail.path == paths[1],
                "saved input paths differ"
            );
            [Side::restore(state.sides[0].clone())?, Side::restore(state.sides[1].clone())?]
        } else {
            paths.map(|p| Side::new(Tail::new(p)))
        };
        let state = match saved {
            Some(state) => {
                state.materialize(&config.out)?;
                state
            }
            None => State {
                version: 1,
                sides: [sides[0].snapshot()?, sides[1].snapshot()?],
                checkpoint: None,
                settled: BTreeMap::new(),
                evidence: BTreeMap::new(),
                emitted: BTreeSet::new(),
                alarms: Vec::new(),
                events: Vec::new(),
                settle: None,
                clock_start: now,
                last_cl: None,
                cl_advanced_at: now,
                cl_headers: 0,
                lag_since: None,
                retry_after: 0,
                rpc_failures: 0,
            },
        };
        Ok(Self {
            sides,
            state,
            out: config.out,
            tripwire: config.tripwire,
            mismatch: None,
            lock,
        })
    }

    pub fn status(&self) -> Status {
        self.state.status(self.sides.each_ref().map(Side::tip))
    }

    pub fn poll(
        &mut self,
        peer: &dyn PeerRpc,
        node: &dyn RdashdRpc,
        now: u64,
        limit: usize,
    ) -> Result<usize> {
        ensure!(limit > 0, "batch limit must be positive");
        self.mismatch = None;
        let previous = self.state.clone();
        let result = self.process(peer, node, now, limit);
        let count = match result {
            Ok(n) => n,
            Err(e) => {
                self.sides = [
                    Side::restore(previous.sides[0].clone())?,
                    Side::restore(previous.sides[1].clone())?,
                ];
                self.state = previous;
                return Err(e);
            }
        };
        self.state.sides = [self.sides[0].snapshot()?, self.sides[1].snapshot()?];
        self.state.save(&self.out)?;
        Ok(count)
    }

    /// Writes the current first mismatch with the existing private-corpus policy.
    pub fn dump_mismatch(
        &self,
        rpc: Option<RpcTarget>,
        dump_dir: Option<&Path>,
    ) -> Result<Option<PathBuf>> {
        let Some(report) = &self.mismatch else {
            return Ok(None);
        };
        let chain = self.sides[0].chain.ok_or_else(|| anyhow!("missing digest chain"))?;
        let embargo = std::env::var_os("DASH_EMBARGO_DIR").map(PathBuf::from);
        let root = dump::root(chain, dump_dir, embargo.as_deref())?;
        let inputs =
            [Input::open(&self.sides[0].tail.path)?, Input::open(&self.sides[1].tail.path)?];
        let extras = Extras {
            context: 3,
            decisions: [None, None],
            rpc,
        };
        dump::write(&root, "shadow", chain, &inputs, report, &extras).map(Some)
    }

    fn alarm(
        &mut self,
        class: Class,
        h: Option<i64>,
        b: Option<String>,
        detail: impl Into<String>,
        now: u64,
    ) {
        let alarm = Alarm {
            class,
            h,
            b,
            detail: detail.into(),
        };
        if self.state.emitted.insert(alarm.key()) {
            if matches!(class, Class::Crit(_)) {
                self.state.clock_start = now;
            }
            self.state.alarms.push(alarm);
        }
    }

    fn process(
        &mut self,
        peer: &dyn PeerRpc,
        node: &dyn RdashdRpc,
        now: u64,
        limit: usize,
    ) -> Result<usize> {
        let mut count = 0;
        for idx in 0..2 {
            let records = self.sides[idx].tail.read(limit)?;
            count += records.len();
            for (at, record) in records {
                self.feed(idx, at, &record, now)?;
            }
        }
        if let [Some(l), Some(r)] = self.sides.each_ref().map(|s| s.chain) {
            ensure!(l == r, "inputs are of different chains");
        }
        self.contradictions(now);
        self.revalidate_settled(now, false);
        if !self.checkpoint_valid(now) {
            return Ok(count);
        }
        let node_status = match node.status() {
            Ok(status) => {
                if status.warnings.contains(&self.tripwire) {
                    self.alarm(Class::Crit(Crit::D3), None, None, &status.warnings, now);
                }
                Some(status)
            }
            Err(e) => {
                self.alarm(Class::Warn(Warn::NodeRpc), None, None, e.to_string(), now);
                None
            }
        };
        if now < self.state.retry_after {
            return Ok(count);
        }
        let status = match peer.status() {
            Ok(s) => s,
            Err(e) => {
                self.rpc_error(e, now);
                return Ok(count);
            }
        };
        if self.state.last_cl.is_none() && self.state.cl_headers == 0 {
            self.state.cl_headers = status.headers;
        }
        if status
            .chainlock
            .as_ref()
            .is_some_and(|cl| self.state.last_cl.as_ref().is_none_or(|old| cl.h > old.h))
        {
            self.state.cl_advanced_at = now;
            self.state.cl_headers = status.headers;
            self.state.last_cl = status.chainlock.clone();
        }
        if let Some(node) = node_status
            && node.chainlock != status.chainlock
        {
            self.alarm(
                Class::Warn(Warn::ChainLockSkew),
                status.chainlock.as_ref().map(|c| c.h),
                status.chainlock.as_ref().map(|c| c.hash.clone()),
                "ChainLock arrival skew",
                now,
            );
        }
        let age = now.saturating_sub(self.state.cl_advanced_at);
        let settlement = settle::choose(
            status.chainlock.as_ref(),
            age,
            self.sides[0].proj.chain(),
            self.sides[1].proj.chain(),
        );
        self.timing(&status, settlement.as_ref(), age, now);
        if settlement.is_none() {
            self.state.settle = None;
        }
        if let Some(settlement) = settlement {
            if self.state.settle.as_ref().map(Settle::mode) != Some(settlement.mode()) {
                self.state.events.push(json!({"event":"settle", "mode":settlement.mode(), "h":settlement.height(), "time":now}));
            }
            self.state.settle = Some(settlement.clone());
            let settled = settlement.height();
            self.classify(settled, now);
            let enriched = self.enrich(peer, status.index_height, now)?;
            self.revalidate_settled(now, true);
            self.drop_checkpoint_pending();
            self.compare(settled.min(status.index_height), now)?;
            self.state.evidence.retain(|hash, e| {
                e.rewound
                    || e.h > settled
                    || self
                        .sides
                        .iter()
                        .any(|side| side.slot(e.h).is_none_or(|slot| slot.b.to_hex() != *hash))
            });
            if !enriched {
                return Ok(count);
            }
        }
        self.state.rpc_failures = 0;
        self.state.retry_after = 0;
        Ok(count)
    }

    fn feed(&mut self, idx: usize, at: usize, record: &Record, now: u64) -> Result<()> {
        let side = &self.sides[idx];
        let rewind_mark = side.proj.rewinds_since(0).len();
        let mut removed = Vec::new();
        if let Record::Outcome(o) = record {
            if o.kind() == Kind::Connect && o.id.org == Origin::Tip && !side.proj.run_state().1 {
                removed.extend(side.proj.chain().iter().filter(|s| s.h >= o.id.h).cloned());
            } else if o.kind() == Kind::Disconnect {
                removed.extend(
                    side.proj.chain().last().filter(|s| s.h == o.id.h && s.b == o.id.b).cloned(),
                );
            }
        }
        self.sides[idx].feed(at, record)?;
        for slot in removed {
            if record.kind() == Kind::Connect
                || self.sides[idx].slot(slot.h).is_none_or(|s| s.b != slot.b)
            {
                let e = self.state.evidence.entry(slot.b.to_hex()).or_default();
                e.h = slot.h;
                e.connected[idx] = true;
                e.rewound = true;
                if record.kind() == Kind::Disconnect {
                    self.state.events.push(json!({"event":"rewind", "side":idx, "from":slot.h, "to":slot.h-1, "at":at, "time":now}));
                    self.rewind_alarm(slot.h, format!("side {idx}: disconnected tip"), now);
                }
            }
        }
        if let Record::Outcome(o) = record
            && (o.kind() == Kind::Reject || (o.kind() == Kind::Connect && o.id.org == Origin::Tip))
        {
            let other_connected = self.sides[1 - idx].slot(o.id.h).is_some_and(|s| s.b == o.id.b);
            let e = self.state.evidence.entry(o.id.b.to_hex()).or_default();
            e.connected[1 - idx] |= other_connected;
            e.h = o.id.h;
            match o.kind() {
                Kind::Connect if o.id.org == Origin::Tip => e.connected[idx] = true,
                Kind::Reject => {
                    e.rejected[idx] = true;
                    e.proposal[idx] |= o.id.org == Origin::Proposal;
                    let res = o.s().get("res").and_then(Value::as_str);
                    e.invalid[idx] |= res == Some("invalid");
                    e.tip_reject[idx] |=
                        o.id.org == Origin::Tip && matches!(res, Some("invalid" | "error"));
                }
                _ => {}
            }
        }
        if let Record::Header(h) = record {
            let run = String::from_utf8_lossy(&h.run);
            let (pid, time) = run.split_once('-').unwrap_or((&run, ""));
            self.state
                .events
                .push(json!({"event":"H", "side":idx, "run":run, "pid":pid, "time":time,
                "version":String::from_utf8_lossy(&h.ver)}));
        }
        let rewinds = self.sides[idx].proj.rewinds_since(rewind_mark).to_vec();
        for rewind in rewinds {
            self.sides[idx].checkpoint_seen = false;
            self.state
                .events
                .push(json!({"event":"rewind", "side":idx, "rewind":rewind, "time":now}));
            self.rewind_alarm(
                rewind.to,
                format!("side {idx}: {} to {}", rewind.from, rewind.to),
                now,
            );
        }
        if let (Some(cp), Record::Outcome(o)) = (&self.state.checkpoint, record)
            && o.kind() == Kind::Connect
            && o.id.org == Origin::Tip
            && o.id.h == cp.h
        {
            self.sides[idx].observe_checkpoint(cp.h);
        }
        Ok(())
    }

    fn rewind_alarm(&mut self, h: i64, detail: String, now: u64) {
        if self.state.settle.as_ref().is_none_or(|s| h > s.height()) {
            self.alarm(Class::Warn(Warn::Rewind), Some(h), None, detail, now);
        }
    }

    fn contradictions(&mut self, now: u64) {
        let contradictions = self
            .state
            .evidence
            .iter()
            .filter(|(_, e)| e.contradicts())
            .map(|(b, e)| (b.clone(), e.h))
            .collect::<Vec<_>>();
        for (b, h) in contradictions {
            self.alarm(
                Class::Crit(Crit::D6),
                Some(h),
                Some(b),
                "tip connect contradicts invalid rejection",
                now,
            );
        }
    }

    fn classify(&mut self, settled: i64, now: u64) {
        for idx in 0..2 {
            let holes = self.sides[idx].proj.holes_since(0).to_vec();
            for hole in holes.into_iter().filter(|h| h.from <= settled) {
                self.alarm(
                    Class::Crit(Crit::D5),
                    Some(hole.from),
                    None,
                    format!("side {idx}: missing heights {}..={}", hole.from, hole.to.min(settled)),
                    now,
                );
            }
        }
        let evidence = self.state.evidence.clone();
        for (b, e) in evidence {
            for idx in 0..2 {
                let other = &self.sides[1 - idx];
                if e.tip_reject[idx]
                    && e.h <= settled
                    && other.slot(e.h).is_some_and(|s| s.b.to_hex() == b)
                {
                    self.alarm(
                        Class::Crit(Crit::D2),
                        Some(e.h),
                        Some(b.clone()),
                        "tip rejection of a settled block held by the other side",
                        now,
                    );
                } else if e.rejected[idx]
                    && !e.contradicts()
                    && (e.proposal[idx]
                        || (e.h <= settled
                            && other.slot(e.h).is_none_or(|slot| slot.b.to_hex() != b)))
                {
                    self.alarm(
                        Class::Warn(Warn::Rejection),
                        Some(e.h),
                        Some(b.clone()),
                        "proposal rejection or stale block",
                        now,
                    );
                }
            }
        }
    }

    fn rpc_error(&mut self, e: anyhow::Error, now: u64) {
        self.state.rpc_failures = self.state.rpc_failures.saturating_add(1);
        self.state.retry_after = now.saturating_add((1 << self.state.rpc_failures.min(8)).min(300));
        self.alarm(Class::Warn(Warn::PeerRpc), None, None, e.to_string(), now);
    }

    fn enrich(&mut self, peer: &dyn PeerRpc, index: i64, now: u64) -> Result<bool> {
        let heights =
            self.sides[0].pending.keys().copied().take_while(|h| *h <= index).collect::<Vec<_>>();
        for h in heights {
            let rec = &self.sides[0].pending[&h];
            let golden_fields = group(Kind::Connect, FieldClass::Semantic)
                .iter()
                .filter(|f| f.source == FieldSource::Golden);
            if golden_fields.clone().all(|f| rec.s().get(f.key).is_some()) {
                continue;
            }
            match peer.enrich(rec) {
                Ok(Enrichment::Ready(rec)) => {
                    ensure!(
                        rec.id == self.sides[0].pending[&h].id,
                        "enrichment changed block identity"
                    );
                    self.sides[0].pending.insert(h, rec);
                }
                Ok(Enrichment::Reorg) => return Ok(false),
                Err(e) => {
                    self.rpc_error(e, now);
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn checkpoint_valid(&mut self, now: u64) -> bool {
        if let Some(cp) = self.state.checkpoint.clone() {
            for idx in 0..2 {
                if !self.sides[idx].checkpoint_seen {
                    return false;
                }
                if self.sides[idx].slot(cp.h).is_none_or(|s| s.b.to_hex() != cp.hash) {
                    self.alarm(
                        Class::Crit(Crit::D1),
                        Some(cp.h),
                        Some(cp.hash),
                        "checkpoint left the active chain",
                        now,
                    );
                    return false;
                }
            }
        }
        true
    }

    fn drop_checkpoint_pending(&mut self) {
        if let Some(cp) = &self.state.checkpoint {
            for side in &mut self.sides {
                side.pending = side.pending.split_off(&(cp.h + 1));
            }
        }
    }

    fn revalidate_settled(&mut self, now: u64, check_semantics: bool) {
        let Some(cp) = &self.state.checkpoint else {
            return;
        };
        let heights = self.state.settled.range(..=cp.h).map(|(h, _)| *h).collect::<Vec<_>>();
        let mut mismatches = Vec::new();
        for h in heights {
            let Some(expected) = self.state.settled.get(&h).cloned() else {
                continue;
            };
            for side in &self.sides {
                let Some(outcome) = side.pending.get(&h) else {
                    continue;
                };
                let hash = outcome.id.b.to_hex();
                if hash != expected.hash {
                    mismatches.push((h, hash, Crit::D1, "replayed settled block identity changed"));
                } else if check_semantics && required_missing(outcome) {
                    mismatches.push((
                        h,
                        hash,
                        Crit::D4,
                        "replayed settled required semantic key missing",
                    ));
                } else if check_semantics && fingerprint(outcome) != expected.fingerprint {
                    mismatches.push((h, hash, Crit::D1, "replayed settled semantic value changed"));
                }
            }
        }
        for (h, hash, class, detail) in mismatches {
            self.alarm(Class::Crit(class), Some(h), Some(hash), detail, now);
        }
    }

    fn compare(&mut self, settled: i64, now: u64) -> Result<()> {
        let Some(tip_r) = self.sides[1].tip() else {
            return Ok(());
        };
        let Some(start) = self.state.checkpoint.as_ref().map(|cp| cp.h + 1).or_else(|| {
            self.sides[0]
                .pending
                .keys()
                .next()
                .zip(self.sides[1].pending.keys().next())
                .map(|(l, r)| *l.max(r))
        }) else {
            return Ok(());
        };
        let mut end = start - 1;
        for h in start..=settled.min(tip_r) {
            let [Some(l), Some(_r)] = self.sides.each_ref().map(|s| s.pending.get(&h)) else {
                break;
            };
            if group(Kind::Connect, FieldClass::Semantic)
                .iter()
                .filter(|f| f.source == FieldSource::Golden)
                .any(|f| l.s().get(f.key).is_none())
            {
                break;
            }
            end = h;
        }
        if end < start {
            return Ok(());
        }
        let mut opts = Options::new(Mode::Linear);
        opts.from = Some(start);
        opts.to = Some(end);
        opts.require = group(Kind::Connect, FieldClass::Semantic)
            .iter()
            .filter_map(|f| FieldId::find(Kind::Connect, FieldClass::Semantic, f.key))
            .collect();
        let records = |s: &Side| {
            s.pending
                .range(start..=end)
                .map(|(h, o)| {
                    Ok::<_, anyhow::Error>((s.slot(*h).map_or(0, |slot| slot.at), o.clone()))
                })
                .collect::<Vec<_>>()
        };
        let report = compare_linear(records(&self.sides[0]), records(&self.sides[1]), opts)
            .map_err(|e| anyhow!("{e}"))?;
        let mut clean_end = end;
        if let Some(mismatch) = report.first() {
            let class = match &mismatch.what {
                What::Keys(keys) if keys.iter().any(|k| k.left.is_none() || k.right.is_none()) => {
                    Crit::D4
                }
                What::Gap(_) => Crit::D5,
                _ => Crit::D1,
            };
            self.alarm(
                Class::Crit(class),
                Some(mismatch.h),
                Some(mismatch.b().to_hex()),
                match &mismatch.what {
                    What::Keys(keys) => format!(
                        "semantic keys: {}",
                        keys.iter().map(|k| k.field.to_string()).collect::<Vec<_>>().join(", ")
                    ),
                    What::Identity => "settled block identities differ".to_owned(),
                    What::Gap(_) => "settled record missing".to_owned(),
                },
                now,
            );
            clean_end = mismatch.h - 1;
        } else if report.verdict != Verdict::Clean {
            self.alarm(
                Class::Crit(Crit::D4),
                Some(start),
                None,
                "required comparison coverage is incomplete",
                now,
            );
            return Ok(());
        }
        if report.first().is_some() {
            self.mismatch = Some(report);
        }
        if clean_end >= start {
            let hash = self.sides[0]
                .slot(clean_end)
                .ok_or_else(|| anyhow!("missing checkpoint slot"))?
                .b
                .to_hex();
            self.state.checkpoint = Some(Checkpoint {
                h: clean_end,
                hash,
            });
            self.sides[0].checkpoint_seen = true;
            self.sides[1].checkpoint_seen = true;
            for h in start..=clean_end {
                if let Some(o) = self.sides[0].pending.get(&h) {
                    self.state.settled.insert(
                        h,
                        Settled {
                            hash: o.id.b.to_hex(),
                            fingerprint: fingerprint(o),
                        },
                    );
                }
            }
            for side in &mut self.sides {
                side.pending = side.pending.split_off(&(clean_end + 1));
            }
        }
        Ok(())
    }

    fn timing(&mut self, peer: &PeerStatus, settlement: Option<&Settle>, cl_age: u64, now: u64) {
        let [l, r] = self.sides.each_ref().map(Side::tip);
        if let Some((l, r)) = l.zip(r)
            && l != r
        {
            self.alarm(
                Class::Warn(Warn::TipSkew),
                Some(l.max(r)),
                None,
                format!("tips L={l} R={r}"),
                now,
            );
        }
        let target = peer
            .chainlock
            .as_ref()
            .filter(|cl| self.sides[0].slot(cl.h).is_some_and(|s| s.b.to_hex() == cl.hash))
            .map(|cl| cl.h)
            .or_else(|| settlement.map(Settle::height));
        let lagging = target.is_some_and(|s| r.is_none_or(|r| r < s - 6));
        if lagging {
            let since = *self.state.lag_since.get_or_insert(now);
            let age = now.saturating_sub(since);
            if age > 2 * 60 * 60 {
                self.alarm(
                    Class::Crit(Crit::Stall),
                    r,
                    None,
                    "rdashd below settlement minus six for over two hours",
                    now,
                );
            } else if age > 30 * 60 {
                self.alarm(
                    Class::Warn(Warn::Lag),
                    r,
                    None,
                    "rdashd lag exceeds thirty minutes",
                    now,
                );
            }
        } else {
            self.state.lag_since = None;
        }
        if cl_age > 2 * 60 * 60 && peer.headers > self.state.cl_headers {
            self.alarm(
                Class::Crit(Crit::Stall),
                peer.chainlock.as_ref().map(|c| c.h),
                peer.chainlock.as_ref().map(|c| c.hash.clone()),
                "peer ChainLock stalled while headers grew",
                now,
            );
        } else if cl_age > 30 * 60 && peer.headers > self.state.cl_headers {
            self.alarm(
                Class::Warn(Warn::Lag),
                peer.chainlock.as_ref().map(|c| c.h),
                None,
                "peer ChainLock lag exceeds thirty minutes",
                now,
            );
        }
    }
}

fn fingerprint(outcome: &Outcome) -> Vec<u8> {
    let mut out = Vec::new();
    for field in group(Kind::Connect, FieldClass::Semantic) {
        out.extend_from_slice(field.key.as_bytes());
        out.push(0);
        match outcome.s().get(field.key) {
            Some(value) => {
                out.push(1);
                value.encode(&mut out);
            }
            None => out.push(0),
        }
        out.push(0xff);
    }
    out
}

fn required_missing(outcome: &Outcome) -> bool {
    group(Kind::Connect, FieldClass::Semantic)
        .iter()
        .any(|field| outcome.s().get(field.key).is_none())
}

/// Offline adapter for recorded enrichment and RPC status; live adapters implement `PeerRpc`.
pub struct RecordedPeer {
    pub status: PeerStatus,
    pub golden: BTreeMap<(i64, Hash256), Outcome>,
}

impl RecordedPeer {
    pub fn open(status: PeerStatus, path: &Path) -> Result<Self> {
        let mut golden = BTreeMap::new();
        for item in Records::open(path)? {
            if let (_, Record::Outcome(o)) = item?
                && o.kind() == Kind::Connect
                && o.id.org == Origin::Tip
            {
                golden.insert((o.id.h, o.id.b), o);
            }
        }
        Ok(Self {
            status,
            golden,
        })
    }
}

impl PeerRpc for RecordedPeer {
    fn status(&self) -> Result<PeerStatus> {
        Ok(self.status.clone())
    }
    fn node(&self) -> Result<&dyn NodeRpc> {
        Err(anyhow!("recorded peer has no live RPC"))
    }
    fn enrich(&self, record: &Outcome) -> Result<Enrichment> {
        let Some(golden) = self.golden.get(&(record.id.h, record.id.b)) else {
            return Ok(Enrichment::Reorg);
        };
        let mut rec = record.clone();
        for field in group(Kind::Connect, FieldClass::Semantic)
            .iter()
            .filter(|f| f.source == FieldSource::Golden)
        {
            let value = golden
                .s()
                .get(field.key)
                .ok_or_else(|| anyhow!("recorded golden lacks {}", field.key))?;
            rec.s_mut().set(field.key, value.clone())?;
        }
        Ok(Enrichment::Ready(rec))
    }
}

impl RdashdRpc for NodeStatus {
    fn status(&self) -> Result<NodeStatus> {
        Ok(self.clone())
    }
}

#[cfg(test)]
mod tests;

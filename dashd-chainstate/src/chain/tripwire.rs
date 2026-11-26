//! V-side ChainLock divergence tripwire.
//!
//! The tripwire is diagnostic only: it records rejection evidence and emits
//! [`DivergenceEvent`]s through [`UiNotifications::divergence`](super::UiNotifications::divergence),
//! but never changes a block's status, the chain's selection or any reply. Every hook starts with
//! [`Tripwire::disabled`], and nothing here fails: an inconsistency is logged and skipped.
//!
//! - T1: after ChainLock enforcement and ABC, the floor is still off the active chain and a block
//!   rejected at AcceptBlock or ConnectBlock sits on the floor's path above the fork point.
//! - T2: the best ChainLock has named a block off the active chain for an hour of monotonic time.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use dash_chain::{ChainReader, IndexAccess};
use dash_clock::Clock;
use dash_consensus::{BlockId, BlockValidationError, BlockValidationResult};
use dash_primitives::BlockHash;

use super::actor::V;
use super::locks::ChainLockSig;

/// The capacity of [`RejectedBlocks`].
pub const REJECTED_CAPACITY: usize = 1_024;
/// A1's proximity: an onset is admitted at once when the tip is within this many blocks of the
/// best ChainLock's height.
pub const TRIPWIRE_NEAR_BLOCKS: u32 = 48;
/// A2's wait, in monotonic seconds, before a stall deeper than [`TRIPWIRE_NEAR_BLOCKS`] admits.
pub const TRIPWIRE_DEEP_ADMIT: u64 = 600;
/// T2's deadline after the onset, and the re-alert period of an episode, in monotonic seconds.
pub const TRIPWIRE_DEADLINE: u64 = 3_600;
/// An onset older than this is logged at `info` on each observation.
const ONSET_INFO_AFTER: u64 = 300;
const NO_CONNECT_REASON: &str = "no block on the ChainLocked path connected within 60 min";

/// The tripwire's monotonic clock, in whole seconds.
pub trait MonoSecs: Send + Sync {
    fn mono_secs(&self) -> u64;
}

impl MonoSecs for AtomicU64 {
    fn mono_secs(&self) -> u64 {
        self.load(Ordering::Acquire)
    }
}

/// Where a block was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectionStage {
    /// Context-free CheckBlock on the P2P path: no status change.
    Check,
    /// AcceptBlock's checks: `FAILED_VALID`, no data stored.
    Accept,
    /// ConnectBlock in ABC.
    Connect,
}

impl fmt::Display for RejectionStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Check => "Check",
            Self::Accept => "Accept",
            Self::Connect => "Connect",
        })
    }
}

/// One rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    pub hash: BlockHash,
    pub height: u32,
    /// The reject reason, then the debug message.
    pub reason: String,
    pub stage: RejectionStage,
    pub at_steady: u64,
}

impl Rejection {
    /// `stage: <stage>, <reason>`, an event's reason.
    fn describe(&self) -> String {
        format!("stage: {}, {}", self.stage, self.reason)
    }
}

/// Bounded rejection evidence, oldest evicted first. The map makes removal by id O(1); the FIFO
/// holds `(seq, id)` pairs, and a pair whose `seq` no longer matches the map is skipped, so a
/// replaced entry is never evicted by its old pair.
#[derive(Clone, Debug)]
pub struct RejectedBlocks {
    map: HashMap<BlockId, (u64, Rejection)>,
    fifo: VecDeque<(u64, BlockId)>,
    seq: u64,
}

impl Default for RejectedBlocks {
    fn default() -> Self {
        Self::new()
    }
}

impl RejectedBlocks {
    pub fn new() -> Self {
        Self {
            map: HashMap::with_capacity(REJECTED_CAPACITY),
            fifo: VecDeque::with_capacity(REJECTED_CAPACITY),
            seq: 0,
        }
    }

    pub fn insert(&mut self, id: BlockId, rejection: Rejection) {
        self.seq = self.seq.wrapping_add(1);
        self.map.insert(id, (self.seq, rejection));
        self.fifo.push_back((self.seq, id));
        while self.map.len() > REJECTED_CAPACITY {
            let Some((seq, old)) = self.fifo.pop_front() else {
                break;
            };
            if self.map.get(&old).is_some_and(|(s, _)| *s == seq) {
                self.map.remove(&old);
            }
        }
        if self.fifo.len() > REJECTED_CAPACITY * 2 {
            let map = &self.map;
            self.fifo.retain(|(seq, id)| map.get(id).is_some_and(|(s, _)| s == seq));
        }
    }

    pub fn remove(&mut self, id: BlockId) -> Option<Rejection> {
        self.map.remove(&id).map(|(_, rejection)| rejection)
    }

    pub fn get(&self, id: BlockId) -> Option<&Rejection> {
        self.map.get(&id).map(|(_, rejection)| rejection)
    }

    pub fn iter(&self) -> impl Iterator<Item = (BlockId, &Rejection)> {
        self.map.iter().map(|(id, (_, rejection))| (*id, rejection))
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The FIFO's length, which stays within twice the capacity.
    pub fn fifo_len(&self) -> usize {
        self.fifo.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DivergenceKind {
    T1,
    T2,
}

/// One divergence event's facts. The node's marker writer sets `first_event_unix` and `seq`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DivergenceInfo {
    /// `<kind>-<height>-<hash12>` of the episode's first event: the floor for T1, the frozen key
    /// for T2.
    pub episode: String,
    pub kind: DivergenceKind,
    /// The rejection's stage; none for a T2 event with no rejection on its path.
    pub stage: Option<RejectionStage>,
    /// The rejected block's height and hash for T1, and for a T2 event whose reason names an
    /// entry on its path; otherwise the best ChainLock's block.
    pub height: u32,
    pub hash: BlockHash,
    /// The block the best ChainLock names.
    pub cl_hash: BlockHash,
    /// The floor's block at the event, if any.
    pub floor_hash: Option<BlockHash>,
    /// A T2 episode's frozen onset ChainLock block; none for T1.
    pub onset_cl: Option<BlockHash>,
    /// The best ChainLock's serialized BLS signature.
    pub clsig: [u8; 96],
    pub reason: String,
    pub h_obs: u32,
    /// Whether this event was re-armed from a marker at startup; a later event of a restored
    /// episode is `false`.
    pub restored: bool,
    pub first_event_unix: i64,
    pub seq: u64,
}

impl DivergenceInfo {
    /// The episode join hashes: `cl_hash`, `floor_hash` and `onset_cl`, each when present, and
    /// the rejected block's `hash` for T1 and for a T2 event whose reason names an entry (a
    /// `stage`).
    fn hashes(&self) -> impl Iterator<Item = BlockHash> + '_ {
        let rejected =
            (self.kind == DivergenceKind::T1 || self.stage.is_some()).then_some(self.hash);
        [Some(self.cl_hash), self.floor_hash, self.onset_cl, rejected].into_iter().flatten()
    }

    /// Whether the two share a present hash (an absent hash matches nothing).
    fn joins(&self, other: &DivergenceInfo) -> bool {
        self.hashes().any(|a| other.hashes().any(|b| a == b))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DivergenceEvent {
    /// An episode's first event: the node writes the marker, sets the warning and alerts.
    First(DivergenceInfo),
    /// A later event of a live episode, at most hourly.
    Realert(DivergenceInfo),
    /// A restored episode's ChainLock seen again at intake, once per process.
    Redelivered(DivergenceInfo),
}

/// The first event, for M7's signing seam.
#[derive(Clone, Debug, Default)]
pub struct DivergenceLatch(Arc<OnceLock<DivergenceInfo>>);

impl DivergenceLatch {
    pub fn tripped(&self) -> Option<&DivergenceInfo> {
        self.0.get()
    }

    fn set(&self, info: &DivergenceInfo) {
        let _ = self.0.set(info.clone());
    }
}

#[derive(Clone, Debug)]
struct Episode {
    /// The first event (or the restored marker's).
    info: DivergenceInfo,
    last_alert: Option<u64>,
    redelivered: bool,
    /// Every event that belonged to it, the first included, and how many of them were T1's.
    events: u64,
    t1_events: u64,
}

/// A T2 onset, frozen when admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Onset {
    /// Monotonic seconds; backdated to `deep.since` under A2.
    pub since: u64,
    /// The block the best ChainLock named at admission.
    pub cl: BlockHash,
    /// `lca(tip, CL block)` at admission (our tip if the header was unknown).
    pub key: (u32, BlockHash),
    pub h_obs: u32,
}

/// A2's deep stall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Deep {
    h_max: u32,
    since: u64,
    key: (u32, BlockHash),
}

/// What one observation saw of V.
pub(crate) struct Observation<'a> {
    pub reader: &'a ChainReader,
    pub tip: Option<BlockId>,
    pub floor: Option<BlockId>,
    pub best: Option<&'a ChainLockSig>,
    pub enabled: bool,
    pub ibd: bool,
    pub now: u64,
}

#[derive(Debug)]
pub struct Tripwire {
    enabled: bool,
    rejected: RejectedBlocks,
    started: Instant,
    mono: Option<Arc<dyn MonoSecs>>,
    onset: Option<Onset>,
    deep: Option<Deep>,
    h_obs: u32,
    episodes: Vec<Episode>,
    /// The live T1 episode and the floor of its first T1 event, which resolves it.
    t1_episode: Option<(usize, BlockHash)>,
    /// The episode of the current onset's T2 events.
    t2_episode: Option<usize>,
    latch: DivergenceLatch,
    /// The last mismatched best ChainLock logged; the best only moves forward.
    mismatch_logged: Option<BlockHash>,
    connect_hook: bool,
    lookups: u64,
}

impl fmt::Debug for dyn MonoSecs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MonoSecs({})", self.mono_secs())
    }
}

impl Tripwire {
    pub fn armed() -> Self {
        Self::new(true)
    }

    /// A tripwire whose hooks do nothing and read nothing.
    pub fn disabled_tripwire() -> Self {
        Self::new(false)
    }

    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            rejected: RejectedBlocks::new(),
            started: Instant::now(),
            mono: None,
            onset: None,
            deep: None,
            h_obs: 0,
            episodes: Vec::new(),
            t1_episode: None,
            t2_episode: None,
            latch: DivergenceLatch::default(),
            mismatch_logged: None,
            connect_hook: true,
            lookups: 0,
        }
    }

    pub fn disabled(&self) -> bool {
        !self.enabled
    }

    /// The production clock's base: `Clock::steady` when V spawns.
    pub fn set_started(&mut self, started: Instant) {
        self.started = started;
    }

    /// Replaces the production clock (`Clock::steady` since V started).
    pub fn set_mono(&mut self, mono: Arc<dyn MonoSecs>) {
        self.mono = Some(mono);
    }

    pub fn latch(&self) -> DivergenceLatch {
        self.latch.clone()
    }

    pub fn tripped(&self) -> Option<&DivergenceInfo> {
        self.latch.tripped()
    }

    pub fn rejected(&self) -> &RejectedBlocks {
        &self.rejected
    }

    pub fn onset(&self) -> Option<Onset> {
        self.onset
    }

    /// The number of episodes, restored ones included.
    pub fn episode_count(&self) -> usize {
        self.episodes.len()
    }

    /// Each episode's name, its events and its T1 events, in episode order.
    pub fn episode_events(&self) -> Vec<(String, u64, u64)> {
        self.episodes.iter().map(|e| (e.info.episode.clone(), e.events, e.t1_events)).collect()
    }

    /// When A2's deep stall started, if one is being timed.
    pub fn deep_since(&self) -> Option<u64> {
        self.deep.map(|deep| deep.since)
    }

    /// The live T1 episode's name.
    pub fn t1_episode(&self) -> Option<&str> {
        let (index, _) = self.t1_episode?;
        self.episodes.get(index).map(|e| e.info.episode.as_str())
    }

    /// The skip-list lookups of the last T1 evaluation.
    pub fn lookups(&self) -> u64 {
        self.lookups
    }

    /// Bypasses the connect hook's removal, for tests that show the T1 entry test suffices on its
    /// own.
    #[cfg(test)]
    pub(crate) fn set_connect_hook(&mut self, on: bool) {
        self.connect_hook = on;
    }

    /// Seeds an episode from a restored marker: its first alert and bundle allowance are spent.
    pub fn restore(&mut self, mut info: DivergenceInfo) {
        if self.disabled() {
            return;
        }
        info.restored = true;
        self.latch.set(&info);
        self.episodes.push(Episode {
            info,
            last_alert: None,
            redelivered: false,
            events: 0,
            t1_events: 0,
        });
    }

    /// A successful ConnectTip of `id`: its rejection is stale.
    pub fn connected(&mut self, id: BlockId) {
        if self.disabled() || !self.connect_hook {
            return;
        }
        self.rejected.remove(id);
    }

    /// Records the rejection of indexed block `id`; Mutated failures are never recorded.
    pub(crate) fn reject(
        &mut self,
        reader: &ChainReader,
        id: BlockId,
        stage: RejectionStage,
        err: &BlockValidationError,
        now: u64,
    ) {
        if self.disabled() {
            return;
        }
        match reader.entry(id) {
            Some(e) => self.record(id, e.hash, e.height, stage, err, now),
            None => tracing::debug!("tripwire: rejected block {id:?} has no entry"),
        }
    }

    /// [`Tripwire::reject`] with the block's hash and height given.
    pub(crate) fn record(
        &mut self,
        id: BlockId,
        hash: BlockHash,
        height: u32,
        stage: RejectionStage,
        err: &BlockValidationError,
        now: u64,
    ) {
        if self.disabled() || err.result() == BlockValidationResult::Mutated {
            return;
        }
        let reason = match err.debug_message() {
            "" => err.reject_reason().to_owned(),
            debug => format!("{}, {debug}", err.reject_reason()),
        };
        self.rejected.insert(
            id,
            Rejection {
                hash,
                height,
                reason,
                stage,
                at_steady: now,
            },
        );
    }

    pub fn mono_secs(&self, clock: &dyn Clock) -> u64 {
        match &self.mono {
            Some(mono) => mono.mono_secs(),
            None => clock.steady().saturating_duration_since(self.started).as_secs(),
        }
    }

    /// The redelivery match at ChainLock intake: every restored episode holding `hash` alerts
    /// once, each starting its own re-alert cadence.
    pub fn redelivery(&mut self, hash: BlockHash, now: u64) -> Vec<DivergenceEvent> {
        if self.disabled() {
            return Vec::new();
        }
        self.episodes
            .iter_mut()
            .filter(|e| {
                e.info.restored
                    && !e.redelivered
                    && e.info.hashes().any(|candidate| candidate == hash)
            })
            .map(|episode| {
                episode.redelivered = true;
                episode.last_alert = Some(now);
                DivergenceEvent::Redelivered(episode.info.clone())
            })
            .collect()
    }

    /// Whether `rejection` is on `path`'s chain above its fork from `tip`. Counts its lookups.
    fn on_path(
        lookups: &mut u64,
        reader: &ChainReader,
        path: BlockId,
        tip: Option<BlockId>,
        id: BlockId,
        rejection: &Rejection,
    ) -> bool {
        *lookups += 1;
        if reader.ancestor(path, rejection.height) != Some(id) {
            return false;
        }
        *lookups += 1;
        tip.map(|tip| reader.ancestor(tip, rejection.height)) != Some(Some(id))
    }

    fn rejection_on_path(
        &mut self,
        reader: &ChainReader,
        path: BlockId,
        tip: Option<BlockId>,
        include_check: bool,
    ) -> Option<(BlockId, Rejection)> {
        self.lookups = 0;
        let lookups = &mut self.lookups;
        // The lowest entry on the path, Accept and Connect before Check: deterministic, and the
        // first block where the chains part.
        self.rejected
            .iter()
            .filter(|(_, r)| include_check || r.stage != RejectionStage::Check)
            .filter(|(id, r)| Self::on_path(lookups, reader, path, tip, *id, r))
            .min_by_key(|(_, r)| (r.stage == RejectionStage::Check, r.height))
            .map(|(id, r)| (id, r.clone()))
    }

    fn live(&self, index: usize, episode: &Episode) -> bool {
        episode.info.restored
            || self.t1_episode.is_some_and(|(i, _)| i == index)
            || self.t2_episode == Some(index)
    }

    /// The first live T2 episode, fresh or restored, whose onset ChainLock's block is indexed and
    /// has `id` as its ancestor at `height`.
    fn onset_path_join(&self, reader: &ChainReader, id: BlockId, height: u32) -> Option<usize> {
        self.episodes.iter().enumerate().position(|(i, e)| {
            e.info.kind == DivergenceKind::T2
                && self.live(i, e)
                && e.info
                    .onset_cl
                    .and_then(|cl| reader.lookup(&cl))
                    .is_some_and(|cl| reader.ancestor(cl, height) == Some(id))
        })
    }

    /// Emits `info` into `frozen`, else into a live episode it joins, else into `on_path`, else
    /// as a new episode. Returns the episode's index and the event to send, if any.
    fn emit(
        &mut self,
        mut info: DivergenceInfo,
        frozen: Option<usize>,
        on_path: Option<usize>,
        now: u64,
    ) -> (usize, Option<DivergenceEvent>) {
        let joined = frozen
            .filter(|&i| i < self.episodes.len())
            .or_else(|| {
                self.episodes
                    .iter()
                    .enumerate()
                    .position(|(i, e)| self.live(i, e) && e.info.joins(&info))
            })
            .or(on_path);
        let Some((index, episode)) =
            joined.and_then(|i| self.episodes.get_mut(i).map(|episode| (i, episode)))
        else {
            self.latch.set(&info);
            self.episodes.push(Episode {
                info: info.clone(),
                last_alert: Some(now),
                redelivered: false,
                events: 1,
                t1_events: u64::from(info.kind == DivergenceKind::T1),
            });
            return (self.episodes.len() - 1, Some(DivergenceEvent::First(info)));
        };
        episode.events += 1;
        episode.t1_events += u64::from(info.kind == DivergenceKind::T1);
        info.episode = episode.info.episode.clone();
        if episode.last_alert.is_some_and(|last| now.saturating_sub(last) < TRIPWIRE_DEADLINE) {
            return (index, None);
        }
        episode.last_alert = Some(now);
        (index, Some(DivergenceEvent::Realert(info)))
    }

    fn active(reader: &ChainReader, tip: Option<BlockId>, id: BlockId) -> bool {
        let Some(height) = reader.entry(id).map(|e| e.height) else {
            return false;
        };
        tip.is_some_and(|tip| reader.ancestor(tip, height) == Some(id))
    }

    fn active_hash(reader: &ChainReader, tip: Option<BlockId>, hash: BlockHash) -> bool {
        reader.lookup(&hash).is_some_and(|id| Self::active(reader, tip, id))
    }

    /// Ends the T1 episode once its frozen floor's block is active, or with the spork off.
    fn resolve_t1(&mut self, reader: &ChainReader, tip: Option<BlockId>, enabled: bool) {
        if self
            .t1_episode
            .is_some_and(|(_, floor)| !enabled || Self::active_hash(reader, tip, floor))
        {
            self.t1_episode = None;
        }
    }

    /// Clears the onset, ending its T2 episode.
    fn clear_onset(&mut self) {
        self.onset = None;
        self.deep = None;
        self.t2_episode = None;
    }

    fn key(reader: &ChainReader, tip: Option<BlockId>, cl: Option<BlockId>) -> (u32, BlockHash) {
        let at = match (tip, cl) {
            (Some(tip), Some(cl)) => reader.lca(tip, cl).or(Some(tip)),
            (tip, _) => tip,
        };
        at.and_then(|id| reader.entry(id))
            .map_or((0, BlockHash::from([0; 32])), |e| (e.height, e.hash))
    }

    fn admit(&mut self, since: u64, cl: BlockHash, key: (u32, BlockHash), h_obs: u32) {
        tracing::debug!("ChainLock divergence: onset at {since} s for {cl}, tip height {h_obs}");
        self.clear_onset();
        self.onset = Some(Onset {
            since,
            cl,
            key,
            h_obs,
        });
    }

    /// T2's observation, at the start of every enforcement (each trigger, the timer's included).
    pub(crate) fn observe(&mut self, o: &Observation<'_>) -> Option<DivergenceEvent> {
        if self.disabled() {
            return None;
        }
        let reader = o.reader;
        self.h_obs = o.tip.and_then(|id| reader.entry(id)).map_or(0, |e| e.height);
        self.resolve_t1(reader, o.tip, o.enabled);
        if !o.enabled || o.ibd {
            self.clear_onset();
            return None;
        }
        let Some(sig) = o.best else {
            self.deep = None;
            return None;
        };
        let cl_id = reader.lookup(&sig.block_hash);
        let indexed = cl_id.and_then(|id| reader.entry(id)).map(|e| e.height);
        let Ok(cl_height) = u32::try_from(sig.height) else {
            self.deep = None;
            return None;
        };
        if let Some(indexed) = indexed.filter(|&h| h != cl_height) {
            if self.mismatch_logged.replace(sig.block_hash) != Some(sig.block_hash) {
                tracing::error!(
                    "ChainLock height mismatch: CLSIG ({sig}) names {}, indexed at height {indexed}",
                    sig.block_hash
                );
            }
            self.clear_onset();
            return None;
        }
        if self.onset.is_some_and(|onset| Self::active_hash(reader, o.tip, onset.cl)) {
            self.clear_onset();
        }
        let holds = !cl_id.is_some_and(|id| Self::active(reader, o.tip, id));
        if !holds {
            self.deep = None;
        } else if self.onset.is_none() {
            let key = Self::key(reader, o.tip, cl_id);
            if self.h_obs >= cl_height.saturating_sub(TRIPWIRE_NEAR_BLOCKS) {
                self.admit(o.now, sig.block_hash, key, self.h_obs);
            } else {
                if self.deep.is_some_and(|deep| self.h_obs > deep.h_max) {
                    self.deep = None;
                }
                let deep = *self.deep.get_or_insert(Deep {
                    h_max: self.h_obs,
                    since: o.now,
                    key,
                });
                if o.now.saturating_sub(deep.since) >= TRIPWIRE_DEEP_ADMIT {
                    self.admit(deep.since, sig.block_hash, deep.key, deep.h_max);
                }
            }
        }
        let onset = self.onset?;
        let age = o.now.saturating_sub(onset.since);
        if age > ONSET_INFO_AFTER {
            tracing::info!("ChainLock divergence: onset for {} is {age} s old", onset.cl);
        }
        if age < TRIPWIRE_DEADLINE {
            return None;
        }
        let rejection = cl_id.and_then(|path| self.rejection_on_path(reader, path, o.tip, true));
        let (stage, reason, height, hash) = match rejection {
            Some((_, r)) => (Some(r.stage), r.describe(), r.height, r.hash),
            None => (None, NO_CONNECT_REASON.to_owned(), cl_height, sig.block_hash),
        };
        let (key_height, key_hash) = onset.key;
        let info = DivergenceInfo {
            episode: episode_name(DivergenceKind::T2, key_height, key_hash),
            kind: DivergenceKind::T2,
            stage,
            height,
            hash,
            cl_hash: sig.block_hash,
            floor_hash: o.floor.and_then(|id| reader.entry(id)).map(|e| e.hash),
            onset_cl: Some(onset.cl),
            clsig: sig.sig.to_bytes(sig.sig.scheme()),
            reason,
            h_obs: onset.h_obs,
            restored: false,
            first_event_unix: 0,
            seq: 0,
        };
        let (index, event) = self.emit(info, self.t2_episode, None, o.now);
        self.t2_episode = Some(index);
        event
    }

    /// T1, after enforcement and ABC left `floor` off the active chain.
    pub(crate) fn evaluate_t1(
        &mut self,
        reader: &ChainReader,
        tip: Option<BlockId>,
        floor: BlockId,
        sig: &ChainLockSig,
        now: u64,
    ) -> Option<DivergenceEvent> {
        if self.disabled() {
            return None;
        }
        self.resolve_t1(reader, tip, true);
        let (rejected, rejection) = self.rejection_on_path(reader, floor, tip, false)?;
        let Some(floor_entry) = reader.entry(floor) else {
            tracing::debug!("tripwire: the floor {floor:?} has no entry");
            return None;
        };
        let info = DivergenceInfo {
            episode: episode_name(DivergenceKind::T1, floor_entry.height, floor_entry.hash),
            kind: DivergenceKind::T1,
            stage: Some(rejection.stage),
            height: rejection.height,
            hash: rejection.hash,
            cl_hash: sig.block_hash,
            floor_hash: Some(floor_entry.hash),
            onset_cl: None,
            clsig: sig.sig.to_bytes(sig.sig.scheme()),
            reason: rejection.describe(),
            h_obs: self.h_obs,
            restored: false,
            first_event_unix: 0,
            seq: 0,
        };
        let frozen = self.t1_episode.map(|(index, _)| index);
        // A T1 whose rejected block lies on a live T2 episode's onset lock path joins it.
        let on_path = match frozen {
            None => self.onset_path_join(reader, rejected, info.height),
            Some(_) => None,
        };
        let (index, event) = self.emit(info, frozen, on_path, now);
        if frozen != Some(index) {
            self.t1_episode = Some((index, floor_entry.hash));
        }
        event
    }
}

fn episode_name(kind: DivergenceKind, height: u32, hash: BlockHash) -> String {
    let hash = hash.to_string();
    format!("{kind:?}-{height}-{}", hash.get(..12).unwrap_or(&hash))
}

#[cfg(test)]
thread_local! {
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LATCHING_IBD_IN_HOOK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Called by V's latching `is_ibd`: counts a call made from inside a tripwire hook.
#[cfg(test)]
pub(crate) fn probe_latching_ibd() {
    if IN_HOOK.with(std::cell::Cell::get) {
        LATCHING_IBD_IN_HOOK.with(|n| n.set(n.get() + 1));
    }
}

/// The latching `is_ibd` calls made from tripwire hooks on this thread (V's).
#[cfg(test)]
pub(crate) fn latching_ibd_in_hook() -> u64 {
    LATCHING_IBD_IN_HOOK.with(std::cell::Cell::get)
}

/// Marks V's thread as inside a tripwire hook, for [`probe_latching_ibd`].
#[cfg(test)]
pub(crate) struct HookScope;

#[cfg(test)]
impl HookScope {
    pub(crate) fn enter() -> Self {
        IN_HOOK.with(|h| h.set(true));
        HookScope
    }
}

#[cfg(test)]
impl Drop for HookScope {
    fn drop(&mut self) {
        IN_HOOK.with(|h| h.set(false));
    }
}

/// V's tripwire hooks.
impl V {
    fn tripwire_now(&self) -> u64 {
        self.tripwire.mono_secs(self.clock.as_ref())
    }

    /// The rejection inserts at P2P CheckBlock, AcceptBlock and ConnectBlock.
    pub(super) fn tripwire_reject(
        &mut self,
        id: BlockId,
        stage: RejectionStage,
        err: &BlockValidationError,
    ) {
        if self.tripwire.disabled() {
            return;
        }
        #[cfg(test)]
        let _scope = HookScope::enter();
        let now = self.tripwire_now();
        self.tripwire.reject(&self.reader, id, stage, err, now);
    }

    /// The observation at the start of enforcement: H_obs, admission and C′ on every trigger.
    pub(super) fn tripwire_observe(&mut self) {
        if self.tripwire.disabled() {
            return;
        }
        #[cfg(test)]
        let _scope = HookScope::enter();
        let o = Observation {
            reader: &self.reader,
            tip: self.tip(),
            floor: self.chain.cl_floor(),
            best: self.cl.sigs.best.as_deref(),
            enabled: self.chain.cl_enabled_observed(),
            ibd: self.chain.is_ibd_private_unlatched(self.now(), self.loading.any()),
            now: self.tripwire_now(),
        };
        if let Some(event) = self.tripwire.observe(&o) {
            self.ui.divergence(&event);
        }
    }

    /// T1, where enforcement returns with the floor still off the active chain.
    pub(super) fn tripwire_t1(&mut self, floor: BlockId) {
        if self.tripwire.disabled() {
            return;
        }
        #[cfg(test)]
        let _scope = HookScope::enter();
        let Some(sig) = self.cl.sigs.best.clone() else {
            return;
        };
        let (tip, now) = (self.tip(), self.tripwire_now());
        if let Some(event) = self.tripwire.evaluate_t1(&self.reader, tip, floor, &sig, now) {
            self.ui.divergence(&event);
        }
    }

    /// The redelivery match at ChainLock intake.
    pub(super) fn tripwire_intake(&mut self, hash: BlockHash) {
        if self.tripwire.disabled() {
            return;
        }
        #[cfg(test)]
        let _scope = HookScope::enter();
        let now = self.tripwire_now();
        for event in self.tripwire.redelivery(hash, now) {
            self.ui.divergence(&event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> BlockHash {
        BlockHash::from([byte; 32])
    }

    fn info(
        kind: DivergenceKind,
        cl_hash: BlockHash,
        floor_hash: Option<BlockHash>,
        onset_cl: Option<BlockHash>,
    ) -> DivergenceInfo {
        DivergenceInfo {
            episode: format!("{kind:?}-{cl_hash}"),
            kind,
            stage: None,
            height: 1,
            hash: cl_hash,
            cl_hash,
            floor_hash,
            onset_cl,
            clsig: [0; 96],
            reason: "reason".to_owned(),
            h_obs: 1,
            restored: false,
            first_event_unix: 0,
            seq: 0,
        }
    }

    fn rejection(n: usize) -> Rejection {
        Rejection {
            hash: hash((n & 0xff) as u8),
            height: n as u32,
            reason: "reason".to_owned(),
            stage: RejectionStage::Connect,
            at_steady: n as u64,
        }
    }

    #[test]
    fn rejected_blocks_evict_oldest_and_stay_bounded() {
        let mut rejected = RejectedBlocks::new();
        for n in 0..=REJECTED_CAPACITY {
            rejected.insert(BlockId(n as u32), rejection(n));
        }
        assert_eq!(rejected.len(), REJECTED_CAPACITY);
        assert!(rejected.get(BlockId(0)).is_none(), "the oldest is evicted");
        assert!(rejected.remove(BlockId(REJECTED_CAPACITY as u32)).is_some());
        // Re-inserting one id keeps its newest value: its old FIFO pair is skipped.
        for n in 0..10 * REJECTED_CAPACITY {
            rejected.insert(BlockId(7), rejection(n));
            assert!(rejected.fifo_len() <= 2 * REJECTED_CAPACITY + 1);
        }
        assert_eq!(rejected.get(BlockId(7)).map(|r| r.height), Some(10 * 1_024 - 1));
    }

    #[test]
    fn restored_redelivery_matches_each_present_hash_once() {
        let mut tripwire = Tripwire::armed();
        tripwire.restore(info(DivergenceKind::T2, hash(1), None, Some(hash(2))));
        assert!(matches!(&tripwire.redelivery(hash(2), 10)[..], [DivergenceEvent::Redelivered(_)]));
        assert!(tripwire.redelivery(hash(2), 20).is_empty());
        assert!(tripwire.redelivery(hash(1), 20).is_empty());
        assert!(tripwire.redelivery(hash(3), 30).is_empty());
    }

    #[test]
    fn absent_hashes_never_join_an_episode() {
        let mut tripwire = Tripwire::armed();
        tripwire.restore(info(DivergenceKind::T2, hash(1), None, None));
        let (_, event) =
            tripwire.emit(info(DivergenceKind::T2, hash(2), None, None), None, None, 0);
        assert!(matches!(event, Some(DivergenceEvent::First(_))));
        assert_eq!(tripwire.episode_count(), 2);
    }

    #[test]
    fn a_t1_rejected_hash_joins_and_redelivers() {
        let mut tripwire = Tripwire::armed();
        tripwire.restore(info(DivergenceKind::T2, hash(1), None, Some(hash(2))));
        let mut t1 = info(DivergenceKind::T1, hash(3), Some(hash(3)), None);
        t1.hash = hash(2);
        let (_, event) = tripwire.emit(t1, None, None, 0);
        assert!(matches!(event, Some(DivergenceEvent::Realert(_))));
        assert_eq!(tripwire.episode_count(), 1);

        let mut tripwire = Tripwire::armed();
        let mut t1 = info(DivergenceKind::T1, hash(3), Some(hash(3)), None);
        t1.hash = hash(4);
        tripwire.restore(t1);
        assert!(matches!(&tripwire.redelivery(hash(4), 0)[..], [DivergenceEvent::Redelivered(_)]));

        let mut tripwire = Tripwire::armed();
        let mut t2 = info(DivergenceKind::T2, hash(5), None, Some(hash(6)));
        t2.hash = hash(7);
        tripwire.restore(t2);
        assert!(
            tripwire.redelivery(hash(7), 0).is_empty(),
            "a T2 event's hash with no stage is no join hash"
        );

        let mut tripwire = Tripwire::armed();
        let mut t2 = info(DivergenceKind::T2, hash(5), None, Some(hash(6)));
        t2.hash = hash(7);
        t2.stage = Some(RejectionStage::Check);
        tripwire.restore(t2);
        assert!(
            matches!(&tripwire.redelivery(hash(7), 0)[..], [DivergenceEvent::Redelivered(_)]),
            "a T2 event's rejected block is a join hash"
        );
    }

    #[test]
    fn a_redelivery_alerts_every_matching_restored_episode() {
        let mut tripwire = Tripwire::armed();
        tripwire.restore(info(DivergenceKind::T2, hash(1), None, Some(hash(2))));
        tripwire.restore(info(DivergenceKind::T1, hash(3), Some(hash(2)), None));
        tripwire.restore(info(DivergenceKind::T2, hash(4), None, None));
        let events = tripwire.redelivery(hash(2), 10);
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(tripwire.redelivery(hash(2), 20).is_empty());
        // Each started its own cadence at the redelivery: no re-alert before an hour after it.
        for index in 0..2 {
            let (_, event) = tripwire.emit(
                info(DivergenceKind::T2, hash(9), None, None),
                Some(index),
                None,
                10 + TRIPWIRE_DEADLINE - 1,
            );
            assert!(event.is_none(), "{index}");
            let (_, event) = tripwire.emit(
                info(DivergenceKind::T2, hash(9), None, None),
                Some(index),
                None,
                10 + TRIPWIRE_DEADLINE,
            );
            assert!(matches!(event, Some(DivergenceEvent::Realert(_))), "{index}");
        }
    }

    #[test]
    fn a_reason_has_the_debug_message_only_when_there_is_one() {
        let mut tripwire = Tripwire::armed();
        let bare = BlockValidationError::new(BlockValidationResult::Consensus, "bad-planted");
        tripwire.record(BlockId(1), hash(1), 1, RejectionStage::Connect, &bare, 0);
        let rejected = tripwire.rejected().get(BlockId(1)).map(|r| r.reason.clone());
        assert_eq!(rejected.as_deref(), Some("bad-planted"));
        let debug = bare.with_debug("at height 1");
        tripwire.record(BlockId(2), hash(2), 2, RejectionStage::Connect, &debug, 0);
        let rejected = tripwire.rejected().get(BlockId(2)).map(|r| r.reason.clone());
        assert_eq!(rejected.as_deref(), Some("bad-planted, at height 1"));
    }

    #[test]
    fn a_disabled_tripwire_records_nothing() {
        let mut tripwire = Tripwire::disabled_tripwire();
        let err = BlockValidationError::new(BlockValidationResult::Consensus, "bad");
        tripwire.record(BlockId(1), hash(1), 1, RejectionStage::Connect, &err, 0);
        tripwire.restore(info(DivergenceKind::T1, hash(1), None, None));
        assert!(tripwire.rejected().is_empty());
        assert!(tripwire.tripped().is_none());
        assert!(tripwire.redelivery(hash(1), 0).is_empty());
    }
}

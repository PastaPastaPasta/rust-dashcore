//! Initial anchor selection and coverage transitions, without storage or networking.

use std::net::SocketAddr;
use std::sync::Arc;

use dash_chain::{
    anchor_candidate, anchor_fallback, Anchor, AnchorPack, AnchorState, BlockRef, ChainMeta,
    ClPersisted, CoverageMiss, HeaderStore, StoreError, CHAIN_SCHEMA,
};
use dash_consensus::{BlockId, Header};
use dash_mnlist::{Coverage, LlmqConfig, RequestKey};
use dash_primitives::BlockHash;

use super::ClientConfig;
use crate::error::SpvError;

/// The anchor and the 24 headers below it.
const WINDOW: u32 = 25;

/// Why the store's anchor cannot be opened. Every one fails the open: nothing is reset.
#[derive(Debug, thiserror::Error)]
pub enum StoredAnchorError {
    #[error("reading the stored anchor window: {0}")]
    Storage(#[from] StoreError),
    #[error("the stored anchor {hash:?} at height {height} has no record")]
    MissingAnchor {
        height: u32,
        hash: BlockHash,
    },
    /// The stored header does not hash to the hash stored with it.
    #[error(
        "the stored header at height {height} hashes to {computed:?}, not its stored {stored:?}"
    )]
    Rehash {
        height: u32,
        stored: BlockHash,
        computed: BlockHash,
    },
    /// The window is not the stored anchor's parent chain down to anchor − 24.
    #[error("the stored anchor window breaks at height {height}")]
    Broken {
        height: u32,
    },
    #[error(
        "the stored Established anchor at height {height} has no anchor window and no anchor \
         pack at or below it: wipe the storage and resync"
    )]
    LegacyAnchor {
        height: u32,
    },
}

/// What the host reads from the store at open: `meta`, and an Established checkpoint anchor's
/// window, every (header, hash) pair re-hashed.
#[derive(Clone, Debug)]
pub struct StoredChain {
    meta: ChainMeta,
    window: StoredWindow,
}

#[derive(Clone, Debug)]
enum StoredWindow {
    /// Not read: a genesis anchor, or a Candidate, which opens from its pack.
    Unread,
    /// The anchor's record with no parent stored: a store from before the window was kept.
    AnchorOnly,
    Held(Anchor),
}

impl StoredChain {
    /// `None` for a store with no `meta`. An Established checkpoint anchor's window is
    /// [`HeaderStore::walk_down`] from the anchor's record. Each pair is re-hashed and must
    /// name its child's parent, the top one the meta's anchor; a broken or partial window fails.
    pub fn read(store: &impl HeaderStore) -> Result<Option<StoredChain>, StoredAnchorError> {
        let Some(meta) = store.chain_meta()? else {
            return Ok(None);
        };
        let window = match (meta.anchor_state, meta.anchor) {
            (AnchorState::Established, (hash, height)) if height != 0 => {
                read_window(store, hash, height)?
            }
            _ => StoredWindow::Unread,
        };
        Ok(Some(StoredChain {
            meta,
            window,
        }))
    }

    pub fn meta(&self) -> &ChainMeta {
        &self.meta
    }
}

fn read_window(
    store: &impl HeaderStore,
    hash: BlockHash,
    height: u32,
) -> Result<StoredWindow, StoredAnchorError> {
    let top = store.by_hash(&hash)?.ok_or(StoredAnchorError::MissingAnchor {
        height,
        hash,
    })?;
    let records = store.walk_down(top.id, WINDOW)?;
    let mut child = hash;
    for (below, rec) in (0..).zip(&records) {
        let computed = Header::new(rec.header).hash();
        if computed != rec.hash {
            return Err(StoredAnchorError::Rehash {
                height: rec.height,
                stored: rec.hash,
                computed,
            });
        }
        if computed != child || height.checked_sub(below) != Some(rec.height) {
            return Err(StoredAnchorError::Broken {
                height: rec.height,
            });
        }
        child = rec.header.prev_hash();
    }
    match (records.len(), records.first()) {
        (1, _) => Ok(StoredWindow::AnchorOnly),
        (n, Some(top)) if n == WINDOW as usize => {
            let mut window = Box::new([top.header; WINDOW as usize]);
            for (slot, rec) in window.iter_mut().rev().zip(&records) {
                *slot = rec.header;
            }
            Ok(StoredWindow::Held(Anchor::Checkpoint {
                height,
                hash,
                chain_work: top.chain_work,
                window,
            }))
        }
        (n, _) => Err(StoredAnchorError::Broken {
            height: height.saturating_sub(n as u32),
        }),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Unopened,
    Candidate {
        anchor: Anchor,
    },
    Coverage {
        anchor: Anchor,
    },
    /// `Establish { id, .. }` is outstanding; only its matched result leaves this phase.
    Establishing {
        id: u64,
        anchor: Anchor,
        tip: BlockRef,
        tip_gen: u64,
        /// The latest header input since the command, acted on after a `Stale` result.
        recorded: Option<HeaderState>,
        /// The latest unrequested replacement since the command, applied on the result.
        replaced: Option<Anchor>,
    },
    Established,
    Failed,
}

/// The header task's sync state. `tip_gen` rises with every published tip that differs from
/// the last, a same-height reorg and a new chain included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderState {
    CaughtUp {
        tip: BlockRef,
        tip_gen: u64,
    },
    Behind {
        tip_gen: u64,
    },
}

impl HeaderState {
    pub fn tip_gen(&self) -> u64 {
        match *self {
            HeaderState::CaughtUp {
                tip_gen,
                ..
            }
            | HeaderState::Behind {
                tip_gen,
            } => tip_gen,
        }
    }
}

/// The host's answer to one `Establish`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EstablishOutcome {
    /// The guarded establish wrote the Durable `Established`.
    Established,
    /// The guard failed and wrote nothing; the header task's current state.
    Stale(HeaderState),
    /// The establish failed and the client stops.
    Failed(String),
}

#[derive(Clone, Debug)]
pub enum Input {
    Opened(Option<StoredChain>),
    HeadersCaughtUp {
        tip: BlockRef,
        tip_gen: u64,
    },
    HeadersBehind {
        tip_gen: u64,
    },
    /// The result of the `RunCoverage` with these generations.
    Coverage {
        result: Coverage,
        tip_gen: u64,
        cand_gen: u64,
    },
    /// The pump ended a round for one of coverage's requests: every eligible peer in `tried`
    /// failed it. The pump keeps the request and its cooldown.
    CoverageRoundSpent {
        key: RequestKey,
        tried: Vec<SocketAddr>,
        tip_gen: u64,
        cand_gen: u64,
    },
    /// Exactly one per `Establish`, with its `id` and `cand_gen`.
    EstablishResult {
        id: u64,
        cand_gen: u64,
        outcome: EstablishOutcome,
    },
    /// The header task replaced the chain at a generation the driver did not request, and has
    /// already reset the store and built the new chain.
    Replaced {
        anchor: Anchor,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// The host durably resets the store before building the new chain.
    Reset(ChainMeta),
    BuildChain(Anchor),
    SyncHeaders,
    /// Coverage on one pin of `tip`; its result echoes both generations. On `Pending` the pump
    /// sends the engine's requests, and owns every attempt and cooldown.
    RunCoverage {
        tip: BlockRef,
        tip_gen: u64,
        cand_gen: u64,
    },
    /// The guarded establish on the pin `tip_gen` names; the host answers `EstablishResult`
    /// with this `id` and `cand_gen`, once.
    Establish {
        id: u64,
        tip: BlockRef,
        tip_gen: u64,
        cand_gen: u64,
    },
    /// MN off: the header task's own establish at open, before any header sync.
    EstablishUnguarded,
    StartSubscribers,
    Fallback(Anchor),
    /// The client stops: the establish failed.
    FailStop(String),
    Diagnostic(String),
}

pub struct InitialSync {
    phase: Phase,
    packs: Vec<AnchorPack>,
    llmq: Arc<LlmqConfig>,
    config: ClientConfig,
    birth: Option<u32>,
    tip: Option<BlockRef>,
    tip_gen: Option<u64>,
    /// `tip_gen` when the current candidate replaced the last one: header inputs at or below it
    /// describe the old chain.
    replaced_at: Option<u64>,
    cand_gen: u64,
    behind: bool,
    /// Pump rounds spent on coverage's requests, for status only.
    rounds_spent: u64,
    /// Never reused in the driver's life.
    next_establish_id: u64,
}

impl InitialSync {
    /// Creates an unwired driver. `birth` is the host's resolved wallet birth height.
    pub fn new(
        config: ClientConfig,
        llmq: Arc<LlmqConfig>,
        packs: Vec<AnchorPack>,
        birth: Option<u32>,
    ) -> Self {
        Self {
            phase: Phase::Unopened,
            packs,
            llmq,
            config,
            birth,
            tip: None,
            tip_gen: None,
            replaced_at: None,
            cand_gen: 0,
            behind: false,
            rounds_spent: 0,
            next_establish_id: 1,
        }
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// The pump rounds coverage spent: counted, never acted on.
    pub fn coverage_rounds_spent(&self) -> u64 {
        self.rounds_spent
    }

    /// Rises with every `Reset` and every applied `Replaced`.
    pub fn cand_gen(&self) -> u64 {
        self.cand_gen
    }

    pub fn advance(&mut self, input: Input) -> Result<Vec<Output>, SpvError> {
        if let Input::Opened(meta) = input {
            if self.phase != Phase::Unopened {
                return Err(SpvError::Config("initial sync was already opened".into()));
            }
            let mut out = Vec::new();
            let anchor = match meta {
                Some(StoredChain {
                    meta,
                    window,
                }) if meta.anchor_state == AnchorState::Established => {
                    let anchor = self.established_anchor(&meta, window)?;
                    self.phase = Phase::Established;
                    return Ok(vec![
                        Output::BuildChain(anchor),
                        Output::StartSubscribers,
                        Output::SyncHeaders,
                    ]);
                }
                Some(stored) => self.stored_anchor(&stored.meta)?,
                None => {
                    let anchor = anchor_candidate(
                        &self.packs,
                        self.llmq.params(),
                        self.config.start_from_height.or(self.birth),
                        self.config.clock.node_now(),
                        self.llmq.history_blocks(),
                    );
                    out.push(self.reset(&anchor));
                    anchor
                }
            };
            self.phase = Phase::Candidate {
                anchor: anchor.clone(),
            };
            out.push(Output::BuildChain(anchor));
            if self.config.enable_masternodes {
                out.push(Output::SyncHeaders);
            } else {
                self.phase = Phase::Established;
                out.extend([
                    Output::EstablishUnguarded,
                    Output::StartSubscribers,
                    Output::SyncHeaders,
                ]);
            }
            return Ok(out);
        }
        let anchor = match &self.phase {
            Phase::Candidate {
                anchor,
            }
            | Phase::Coverage {
                anchor,
            } => anchor.clone(),
            Phase::Establishing {
                ..
            } => return Ok(self.establishing(input)),
            Phase::Established | Phase::Failed => {
                return Ok(match input {
                    Input::EstablishResult {
                        id,
                        ..
                    } => vec![unmatched(id)],
                    _ => Vec::new(),
                })
            }
            Phase::Unopened => return Err(SpvError::Config("initial sync is not open".into())),
        };
        match input {
            Input::HeadersCaughtUp {
                tip,
                tip_gen,
            } => Ok(self.headers(
                anchor,
                HeaderState::CaughtUp {
                    tip,
                    tip_gen,
                },
            )),
            Input::HeadersBehind {
                tip_gen,
            } => Ok(self.headers(
                anchor,
                HeaderState::Behind {
                    tip_gen,
                },
            )),
            Input::Coverage {
                result,
                tip_gen,
                cand_gen,
            } if self.current(tip_gen, cand_gen) => match result {
                Coverage::Covered => Ok(vec![self.establish(anchor)]),
                Coverage::Pending => Ok(Vec::new()),
                Coverage::Below {
                    lowest,
                } => Ok(self.below(&anchor, lowest)),
            },
            Input::CoverageRoundSpent {
                key,
                tried,
                tip_gen,
                cand_gen,
            } if self.current(tip_gen, cand_gen) => {
                self.rounds_spent += 1;
                Ok(vec![Output::Diagnostic(format!(
                    "coverage request {key:?} spent a pump round over {} peers",
                    tried.len()
                ))])
            }
            Input::EstablishResult {
                id,
                ..
            } => Ok(vec![unmatched(id)]),
            Input::Replaced {
                anchor,
            } => Ok(vec![self.replaced(anchor)]),
            _ => Ok(Vec::new()),
        }
    }

    /// `Covered` on the current pin: the one outstanding `Establish`.
    fn establish(&mut self, anchor: Anchor) -> Output {
        let id = self.next_establish_id;
        self.next_establish_id += 1;
        let tip = self.tip.expect("coverage runs on a tip");
        let tip_gen = self.tip_gen.expect("coverage runs on a tip");
        self.phase = Phase::Establishing {
            id,
            anchor,
            tip,
            tip_gen,
            recorded: None,
            replaced: None,
        };
        Output::Establish {
            id,
            tip,
            tip_gen,
            cand_gen: self.cand_gen,
        }
    }

    /// While `Establishing`: header inputs are recorded, a replacement is kept for the result,
    /// coverage is dropped, and only the result matching `id` and `cand_gen` (never `tip_gen`)
    /// leaves the phase.
    fn establishing(&mut self, input: Input) -> Vec<Output> {
        let Phase::Establishing {
            id: outstanding,
            recorded,
            replaced,
            ..
        } = &mut self.phase
        else {
            unreachable!("only while establishing")
        };
        match input {
            Input::HeadersCaughtUp {
                tip,
                tip_gen,
            } if self.tip_gen.is_none_or(|g| tip_gen >= g) => {
                *recorded = Some(HeaderState::CaughtUp {
                    tip,
                    tip_gen,
                });
                self.tip_gen = Some(tip_gen);
            }
            Input::HeadersBehind {
                tip_gen,
            } if self.tip_gen.is_none_or(|g| tip_gen >= g) => {
                *recorded = Some(HeaderState::Behind {
                    tip_gen,
                });
                self.tip_gen = Some(tip_gen);
            }
            Input::Replaced {
                anchor,
            } => *replaced = Some(anchor),
            Input::EstablishResult {
                id,
                cand_gen,
                outcome,
            } => {
                if id != *outstanding || cand_gen != self.cand_gen {
                    return vec![unmatched(id)];
                }
                return self.established(outcome);
            }
            _ => {}
        }
        Vec::new()
    }

    /// The matched result: a failure stops, a replacement recorded meanwhile is applied, and
    /// otherwise `Established` starts the subscribers and `Stale` resumes coverage on the newer
    /// of its state and the recorded one.
    fn established(&mut self, outcome: EstablishOutcome) -> Vec<Output> {
        let Phase::Establishing {
            anchor,
            recorded,
            replaced,
            ..
        } = std::mem::replace(&mut self.phase, Phase::Failed)
        else {
            unreachable!("only while establishing")
        };
        if let EstablishOutcome::Failed(e) = outcome {
            return vec![Output::FailStop(e)];
        }
        if let Some(next) = replaced {
            return vec![self.replaced(next)];
        }
        let EstablishOutcome::Stale(state) = outcome else {
            self.phase = Phase::Established;
            return vec![Output::StartSubscribers];
        };
        let state = recorded.filter(|r| r.tip_gen() > state.tip_gen()).unwrap_or(state);
        self.phase = Phase::Coverage {
            anchor,
        };
        self.tip_gen = self.tip_gen.max(Some(state.tip_gen()));
        match state {
            HeaderState::CaughtUp {
                tip,
                ..
            } => {
                self.tip = Some(tip);
                self.behind = false;
                vec![self.run_coverage()]
            }
            HeaderState::Behind {
                ..
            } => {
                self.behind = true;
                Vec::new()
            }
        }
    }

    /// The header task already reset the store and built `anchor`'s chain: a new candidate
    /// with no `Reset` or `BuildChain`.
    fn replaced(&mut self, anchor: Anchor) -> Output {
        self.new_candidate(anchor.clone());
        Output::Diagnostic(format!("the chain was replaced; new candidate {anchor:?}"))
    }

    /// A header input in Candidate or Coverage. One from before a reset, or older than the
    /// last, is dropped. `CaughtUp` runs coverage on a new tip, or on the same one after
    /// `Behind`; `Behind` makes Coverage wait for the next `CaughtUp`.
    fn headers(&mut self, anchor: Anchor, state: HeaderState) -> Vec<Output> {
        let tip_gen = state.tip_gen();
        if self.tip_gen.is_some_and(|g| tip_gen < g)
            || self.replaced_at.is_some_and(|g| tip_gen <= g)
        {
            return Vec::new();
        }
        let same = self.tip_gen == Some(tip_gen);
        self.tip_gen = Some(tip_gen);
        let HeaderState::CaughtUp {
            tip,
            ..
        } = state
        else {
            self.behind = true;
            return Vec::new();
        };
        if same && !self.behind {
            return Vec::new();
        }
        self.behind = false;
        self.tip = Some(tip);
        self.phase = Phase::Coverage {
            anchor,
        };
        vec![self.run_coverage()]
    }

    /// Whether a coverage result belongs to the coverage run in progress.
    fn current(&self, tip_gen: u64, cand_gen: u64) -> bool {
        matches!(self.phase, Phase::Coverage { .. })
            && !self.behind
            && self.tip_gen == Some(tip_gen)
            && self.cand_gen == cand_gen
    }

    fn run_coverage(&self) -> Output {
        Output::RunCoverage {
            tip: self.tip.expect("coverage runs on a tip"),
            tip_gen: self.tip_gen.expect("coverage runs on a tip"),
            cand_gen: self.cand_gen,
        }
    }

    fn below(&mut self, anchor: &Anchor, lowest: Option<u32>) -> Vec<Output> {
        let miss = lowest.map_or(CoverageMiss::UnknownBase, CoverageMiss::LowestUncovered);
        let next = anchor_fallback(&self.packs, self.llmq.params(), anchor, miss);
        if next == *anchor {
            return vec![Output::Diagnostic(
                "coverage is below genesis; waiting for headers".into(),
            )];
        }
        let reset = self.reset(&next);
        vec![Output::Fallback(next.clone()), reset, Output::BuildChain(next), Output::SyncHeaders]
    }

    /// A new candidate whose store reset the host runs: [`Self::new_candidate`] and its
    /// `Reset`.
    fn reset(&mut self, anchor: &Anchor) -> Output {
        self.new_candidate(anchor.clone());
        Output::Reset(self.candidate_meta(anchor))
    }

    /// A later `cand_gen`, and no tip until the new chain publishes one.
    fn new_candidate(&mut self, anchor: Anchor) {
        self.cand_gen += 1;
        self.replaced_at = self.tip_gen;
        self.tip = None;
        self.behind = false;
        self.phase = Phase::Candidate {
            anchor,
        };
    }

    fn candidate_meta(&self, anchor: &Anchor) -> ChainMeta {
        let (anchor, root) = match anchor {
            Anchor::Genesis => {
                ((self.llmq.params().consensus.hash_genesis_block.into(), 0), BlockId(0))
            }
            Anchor::Checkpoint {
                hash,
                height,
                window,
                ..
            } => ((*hash, *height), BlockId(window.len() as u32 - 1)),
        };
        ChainMeta {
            schema: CHAIN_SCHEMA,
            anchor,
            anchor_state: AnchorState::Candidate,
            next_id: BlockId(0),
            tip: root,
            best_header: root,
            sync_seq: 0,
            cl: ClPersisted {
                best: None,
                floor: None,
                enabled_observed: false,
            },
        }
    }

    /// An Established store opens from its own window, never from a pack.
    fn established_anchor(
        &self,
        meta: &ChainMeta,
        window: StoredWindow,
    ) -> Result<Anchor, StoredAnchorError> {
        let (hash, height) = meta.anchor;
        match window {
            StoredWindow::Held(anchor) => Ok(anchor),
            StoredWindow::Unread if self.is_genesis(meta) => Ok(Anchor::Genesis),
            StoredWindow::Unread => Err(StoredAnchorError::MissingAnchor {
                height,
                hash,
            }),
            StoredWindow::AnchorOnly => Err(StoredAnchorError::LegacyAnchor {
                height,
            }),
        }
    }

    fn is_genesis(&self, meta: &ChainMeta) -> bool {
        meta.anchor == (self.llmq.params().consensus.hash_genesis_block.into(), 0)
    }

    fn stored_anchor(&self, meta: &ChainMeta) -> Result<Anchor, SpvError> {
        if self.is_genesis(meta) {
            return Ok(Anchor::Genesis);
        }
        let pack =
            self.packs.iter().find(|p| (p.hash, p.height) == meta.anchor).ok_or_else(|| {
                SpvError::Config("stored anchor has no matching anchor pack".into())
            })?;
        Ok(Anchor::Checkpoint {
            height: pack.height,
            hash: pack.hash,
            chain_work: pack.chain_work,
            window: Box::new(pack.headers),
        })
    }
}

/// A result that is not the outstanding command's, dropped.
fn unmatched(id: u64) -> Output {
    Output::Diagnostic(format!("dropped an establish result ({id}) that is not outstanding"))
}

#[cfg(test)]
mod tests {
    use dash_chain::{ClPersisted, HeaderRecord, RecordKind, Status, Work, CHAIN_SCHEMA};
    use dash_clock::MockClock;
    use dash_primitives::{BlockHeader, MerkleRoot};
    use dashcore::Network;

    use super::*;
    use crate::client::llmq::llmq_config;
    use crate::client::DevnetConfig;

    fn driver(mn: bool, birth: Option<u32>) -> InitialSync {
        configured_driver(mn, birth, None, true)
    }

    fn configured_driver(
        mn: bool,
        birth: Option<u32>,
        start: Option<u32>,
        with_packs: bool,
    ) -> InitialSync {
        let mut config =
            ClientConfig::new(Network::Devnet).with_devnet(DevnetConfig::new("sb15-driver"));
        config.enable_masternodes = mn;
        config.start_from_height = start;
        config.clock = Arc::new(MockClock::at(2_000_000));
        let llmq = llmq_config(&config).unwrap();
        let packs = [10000, 20000, 30000]
            .map(|height| {
                pack(
                    height,
                    if height == 30000 {
                        2_000_000
                    } else {
                        1_000_000
                    },
                )
            })
            .to_vec();
        assert!(packs
            .iter()
            .all(|pack| pack.height >= llmq.params().consensus.pow_dgw_height.to_u32() + 24));
        InitialSync::new(
            config,
            llmq,
            if with_packs {
                packs
            } else {
                vec![]
            },
            birth,
        )
    }

    /// 25 headers each naming the one before by its X11, the last at `time`.
    fn pack(height: u32, time: u32) -> AnchorPack {
        let mut headers = [BlockHeader::default(); 25];
        let mut prev = BlockHash::from([0; 32]);
        for (k, header) in (0..).zip(headers.iter_mut()) {
            *header = BlockHeader::new(
                1,
                prev,
                MerkleRoot::from([7; 32]),
                time - (24 - k) * 150,
                0x207f_ffff,
                height,
            );
            prev = Header::new(*header).hash();
        }
        AnchorPack {
            height,
            hash: prev,
            chain_work: Work::from(u64::from(height)),
            headers,
        }
    }

    /// The store's records of a checkpoint anchor's window, the anchor last.
    fn window_records(anchor: &Anchor) -> Vec<HeaderRecord> {
        let Anchor::Checkpoint {
            height,
            chain_work,
            window,
            ..
        } = anchor
        else {
            return Vec::new();
        };
        (0..)
            .zip(window.iter())
            .map(|(k, header)| HeaderRecord {
                id: BlockId(k),
                hash: Header::new(*header).hash(),
                height: height - 24 + k,
                header: *header,
                skip: None,
                status: Status::default(),
                chain_work: if k == 24 {
                    *chain_work
                } else {
                    Work::from(1u64)
                },
                time_max: header.time(),
                kind: if k == 24 {
                    RecordKind::Accepted
                } else {
                    RecordKind::Context
                },
            })
            .collect()
    }

    #[derive(Default)]
    struct Store {
        meta: Option<ChainMeta>,
        records: Vec<HeaderRecord>,
    }

    impl HeaderStore for Store {
        fn by_hash(&self, h: &BlockHash) -> Result<Option<HeaderRecord>, StoreError> {
            Ok(self.records.iter().find(|r| r.hash == *h).cloned())
        }

        fn by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, StoreError> {
            Ok(self.records.iter().find(|r| r.id == id).cloned())
        }

        fn active_at(&self, _: u32) -> Result<Option<BlockId>, StoreError> {
            Ok(None)
        }

        fn from_height(&self, _: u32) -> Result<Vec<HeaderRecord>, StoreError> {
            Ok(Vec::new())
        }

        fn leaves_from(&self, _: BlockId, _: &mut [(BlockId, Work)]) -> Result<usize, StoreError> {
            Ok(0)
        }

        fn children_from(
            &self,
            _: &BlockHash,
            _: BlockId,
            _: &mut [BlockId],
        ) -> Result<usize, StoreError> {
            Ok(0)
        }

        fn chain_meta(&self) -> Result<Option<ChainMeta>, StoreError> {
            Ok(self.meta)
        }

        fn active_from(&self, _: u32) -> Result<Vec<(u32, BlockId)>, StoreError> {
            Ok(Vec::new())
        }

        fn max_id(&self) -> Result<Option<BlockId>, StoreError> {
            Ok(None)
        }
    }

    fn read(meta: ChainMeta, records: Vec<HeaderRecord>) -> Result<StoredChain, StoredAnchorError> {
        let store = Store {
            meta: Some(meta),
            records,
        };
        StoredChain::read(&store).map(|stored| stored.expect("a meta row"))
    }

    /// `meta`, with the anchor's window as the store keeps it.
    fn reopen(meta: ChainMeta, anchor: &Anchor) -> Input {
        Input::Opened(Some(read(meta, window_records(anchor)).unwrap()))
    }

    fn meta(anchor: &Anchor, state: AnchorState, llmq: &LlmqConfig) -> ChainMeta {
        ChainMeta {
            schema: CHAIN_SCHEMA,
            anchor: match anchor {
                Anchor::Genesis => (llmq.params().consensus.hash_genesis_block.into(), 0),
                Anchor::Checkpoint {
                    hash,
                    height,
                    ..
                } => (*hash, *height),
            },
            anchor_state: state,
            next_id: BlockId(99),
            tip: BlockId(98),
            best_header: BlockId(98),
            sync_seq: 7,
            cl: ClPersisted {
                best: None,
                floor: None,
                enabled_observed: false,
            },
        }
    }

    fn pack_anchor(pack: &AnchorPack) -> Anchor {
        Anchor::Checkpoint {
            height: pack.height,
            hash: pack.hash,
            chain_work: pack.chain_work,
            window: Box::new(pack.headers),
        }
    }

    fn anchor_hash(anchor: &Anchor) -> BlockHash {
        match anchor {
            Anchor::Checkpoint {
                hash,
                ..
            } => *hash,
            Anchor::Genesis => unreachable!("a checkpoint"),
        }
    }

    fn tip(n: u8) -> BlockRef {
        BlockRef {
            id: BlockId(u32::from(n)),
            hash: BlockHash::from([n; 32]),
            height: u32::from(n),
        }
    }

    fn caught_up(n: u8, tip_gen: u64) -> Input {
        Input::HeadersCaughtUp {
            tip: tip(n),
            tip_gen,
        }
    }

    fn coverage(result: Coverage, tip_gen: u64, cand_gen: u64) -> Input {
        Input::Coverage {
            result,
            tip_gen,
            cand_gen,
        }
    }

    fn run(n: u8, tip_gen: u64, cand_gen: u64) -> Output {
        Output::RunCoverage {
            tip: tip(n),
            tip_gen,
            cand_gen,
        }
    }

    fn establish(id: u64, n: u8, tip_gen: u64, cand_gen: u64) -> Output {
        Output::Establish {
            id,
            tip: tip(n),
            tip_gen,
            cand_gen,
        }
    }

    fn result(id: u64, cand_gen: u64, outcome: EstablishOutcome) -> Input {
        Input::EstablishResult {
            id,
            cand_gen,
            outcome,
        }
    }

    fn open(driver: &mut InitialSync) -> Anchor {
        let out = driver.advance(Input::Opened(None)).unwrap();
        let Output::BuildChain(anchor) = &out[1] else {
            panic!("{out:?}")
        };
        assert_eq!(
            out,
            vec![
                Output::Reset(driver.candidate_meta(anchor)),
                Output::BuildChain(anchor.clone()),
                Output::SyncHeaders
            ]
        );
        anchor.clone()
    }

    #[test]
    fn candidate_uses_birth_clock_and_genesis_without_packs() {
        let mut d = driver(true, None);
        let a = open(&mut d);
        assert!(matches!(
            a,
            Anchor::Checkpoint {
                height: 20000,
                ..
            }
        ));
        assert_eq!(d.cand_gen(), 1);
        assert!(d.advance(coverage(Coverage::Covered, 0, 1)).unwrap().is_empty());
        assert!(matches!(d.phase(), Phase::Candidate { .. }));
        let mut d = driver(true, Some(15000));
        assert!(matches!(
            open(&mut d),
            Anchor::Checkpoint {
                height: 10000,
                ..
            }
        ));
        let mut d = driver(true, Some(5000));
        assert_eq!(open(&mut d), Anchor::Genesis);
        let mut d = configured_driver(true, None, None, false);
        assert_eq!(open(&mut d), Anchor::Genesis);
        let mut d = configured_driver(true, Some(25000), Some(15000), true);
        assert!(matches!(
            open(&mut d),
            Anchor::Checkpoint {
                height: 10000,
                ..
            }
        ));
    }

    #[test]
    fn stored_candidate_resumes_and_established_never_moves() {
        for state in [AnchorState::Candidate, AnchorState::Established] {
            let mut original = driver(true, None);
            let anchor = open(&mut original);
            let stored = meta(&anchor, state, &original.llmq);
            let mut resumed = driver(true, Some(1));
            let out = resumed.advance(reopen(stored, &anchor)).unwrap();
            let expected = if state == AnchorState::Candidate {
                vec![Output::BuildChain(anchor.clone()), Output::SyncHeaders]
            } else {
                vec![
                    Output::BuildChain(anchor.clone()),
                    Output::StartSubscribers,
                    Output::SyncHeaders,
                ]
            };
            assert_eq!(out, expected);
            assert_eq!(resumed.cand_gen(), 0);
            if state == AnchorState::Established {
                assert_eq!(resumed.phase(), &Phase::Established);
                assert!(resumed
                    .advance(coverage(
                        Coverage::Below {
                            lowest: None
                        },
                        0,
                        0
                    ))
                    .unwrap()
                    .is_empty());
            }
            assert!(resumed.advance(Input::Opened(None)).is_err());
        }
        let mut d = driver(true, None);
        let mut stored = meta(&Anchor::Genesis, AnchorState::Candidate, &d.llmq);
        stored.anchor = (BlockHash::from([99; 32]), 999);
        assert!(d.advance(reopen(stored, &Anchor::Genesis)).is_err());
        assert_eq!(d.phase(), &Phase::Unopened);
    }

    #[test]
    fn coverage_pending_then_covered_establishes_once() {
        let mut d = driver(true, None);
        let anchor = open(&mut d);
        assert_eq!(d.advance(caught_up(8, 1)).unwrap(), vec![run(8, 1, 1)]);
        assert_eq!(
            d.phase(),
            &Phase::Coverage {
                anchor
            }
        );
        assert!(d.advance(coverage(Coverage::Pending, 1, 1)).unwrap().is_empty());
        assert_eq!(
            d.advance(coverage(Coverage::Covered, 1, 1)).unwrap(),
            vec![establish(1, 8, 1, 1)]
        );
        assert!(matches!(
            d.phase(),
            Phase::Establishing {
                id: 1,
                ..
            }
        ));
        assert!(d.advance(coverage(Coverage::Covered, 1, 1)).unwrap().is_empty());
        assert_eq!(
            d.advance(result(1, 1, EstablishOutcome::Established)).unwrap(),
            vec![Output::StartSubscribers]
        );
        assert_eq!(d.phase(), &Phase::Established);
        assert!(d.advance(coverage(Coverage::Covered, 1, 1)).unwrap().is_empty());
    }

    #[test]
    fn below_and_unknown_base_fall_back_and_resume_after_reset() {
        for lowest in [Some(15000), None] {
            let mut d = driver(true, None);
            open(&mut d);
            d.advance(caught_up(8, 1)).unwrap();
            let out = d
                .advance(coverage(
                    Coverage::Below {
                        lowest,
                    },
                    1,
                    1,
                ))
                .unwrap();
            let Output::Fallback(anchor) = &out[0] else {
                panic!("{out:?}")
            };
            assert!(matches!(
                anchor,
                Anchor::Checkpoint {
                    height: 10000,
                    ..
                }
            ));
            assert_eq!(
                out,
                vec![
                    Output::Fallback(anchor.clone()),
                    Output::Reset(d.candidate_meta(anchor)),
                    Output::BuildChain(anchor.clone()),
                    Output::SyncHeaders
                ]
            );
            assert_eq!(d.cand_gen(), 2);
            let stored = meta(anchor, AnchorState::Candidate, &d.llmq);
            let mut reopened = driver(true, None);
            assert_eq!(
                reopened.advance(reopen(stored, anchor)).unwrap(),
                vec![Output::BuildChain(anchor.clone()), Output::SyncHeaders]
            );
            assert_eq!(d.advance(caught_up(8, 2)).unwrap(), vec![run(8, 2, 2)]);
            let out = d
                .advance(coverage(
                    Coverage::Below {
                        lowest: None,
                    },
                    2,
                    2,
                ))
                .unwrap();
            assert_eq!(out[0], Output::Fallback(Anchor::Genesis));
        }
        let mut d = driver(true, None);
        open(&mut d);
        d.advance(caught_up(8, 1)).unwrap();
        let out = d
            .advance(coverage(
                Coverage::Below {
                    lowest: Some(5000),
                },
                1,
                1,
            ))
            .unwrap();
        assert_eq!(out[0], Output::Fallback(Anchor::Genesis));
    }

    #[test]
    fn below_genesis_waits_for_headers() {
        let mut d = configured_driver(true, None, None, false);
        assert_eq!(open(&mut d), Anchor::Genesis);
        d.advance(caught_up(8, 1)).unwrap();
        let out = d
            .advance(coverage(
                Coverage::Below {
                    lowest: None,
                },
                1,
                1,
            ))
            .unwrap();
        assert!(matches!(&out[..], [Output::Diagnostic(_)]), "{out:?}");
        assert_eq!(
            d.phase(),
            &Phase::Coverage {
                anchor: Anchor::Genesis
            }
        );
        assert_eq!(d.cand_gen(), 1);
        assert_eq!(d.advance(caught_up(9, 2)).unwrap(), vec![run(9, 2, 1)]);
    }

    #[test]
    fn stale_headers_and_coverage_after_a_fallback_are_dropped() {
        let mut d = driver(true, None);
        open(&mut d);
        d.advance(caught_up(8, 1)).unwrap();
        d.advance(coverage(
            Coverage::Below {
                lowest: None,
            },
            1,
            1,
        ))
        .unwrap();
        let fallback = Phase::Candidate {
            anchor: pack_anchor(&d.packs[0]),
        };
        assert_eq!(d.phase(), &fallback);
        for stale in [
            caught_up(8, 1),
            Input::HeadersBehind {
                tip_gen: 1,
            },
            caught_up(8, 1),
            coverage(Coverage::Covered, 1, 1),
            coverage(Coverage::Covered, 1, 2),
        ] {
            assert!(d.advance(stale).unwrap().is_empty());
            assert_eq!(d.phase(), &fallback);
        }
        assert_eq!(d.advance(caught_up(5, 2)).unwrap(), vec![run(5, 2, 2)]);
        assert!(d.advance(coverage(Coverage::Covered, 1, 2)).unwrap().is_empty());
        assert_eq!(
            d.advance(coverage(Coverage::Covered, 2, 2)).unwrap(),
            vec![establish(1, 5, 2, 2)]
        );
    }

    #[test]
    fn a_reorg_or_behind_drops_older_coverage() {
        let mut d = driver(true, None);
        open(&mut d);
        d.advance(caught_up(8, 1)).unwrap();
        assert_eq!(d.advance(caught_up(9, 2)).unwrap(), vec![run(9, 2, 1)]);
        assert!(d.advance(caught_up(8, 1)).unwrap().is_empty());
        assert!(d.advance(caught_up(9, 2)).unwrap().is_empty());
        assert!(d.advance(coverage(Coverage::Covered, 1, 1)).unwrap().is_empty());
        assert!(d
            .advance(Input::HeadersBehind {
                tip_gen: 2
            })
            .unwrap()
            .is_empty());
        assert!(d.advance(coverage(Coverage::Covered, 2, 1)).unwrap().is_empty());
        assert!(matches!(d.phase(), Phase::Coverage { .. }));
        assert_eq!(d.advance(caught_up(9, 2)).unwrap(), vec![run(9, 2, 1)]);
        assert_eq!(
            d.advance(coverage(Coverage::Covered, 2, 1)).unwrap(),
            vec![establish(1, 9, 2, 1)]
        );
    }

    fn spent(n: u8, tip_gen: u64, cand_gen: u64) -> Input {
        Input::CoverageRoundSpent {
            key: RequestKey::QrInfo(BlockHash::from([n; 32])),
            tried: vec![SocketAddr::from(([127, 0, 0, 1], 9999))],
            tip_gen,
            cand_gen,
        }
    }

    #[test]
    fn spent_rounds_are_counted_and_never_retry_or_fall_back() {
        let mut d = driver(true, None);
        open(&mut d);
        d.advance(caught_up(8, 1)).unwrap();
        for round in 1..=3u64 {
            for n in [20, 21] {
                let out = d.advance(spent(n, 1, 1)).unwrap();
                assert!(matches!(&out[..], [Output::Diagnostic(_)]), "{out:?}");
            }
            assert_eq!(d.coverage_rounds_spent(), 2 * round);
            assert!(d.advance(coverage(Coverage::Pending, 1, 1)).unwrap().is_empty());
            assert!(matches!(d.phase(), Phase::Coverage { .. }));
        }
        assert!(d.advance(spent(20, 0, 1)).unwrap().is_empty());
        assert!(d.advance(spent(20, 1, 0)).unwrap().is_empty());
        assert_eq!(d.coverage_rounds_spent(), 6);
        assert!(d.advance(caught_up(8, 1)).unwrap().is_empty());
        assert_eq!(d.advance(caught_up(10, 2)).unwrap(), vec![run(10, 2, 1)]);
        assert!(d.advance(spent(20, 1, 1)).unwrap().is_empty());
        assert_eq!(
            d.advance(coverage(Coverage::Covered, 2, 1)).unwrap(),
            vec![establish(1, 10, 2, 1)]
        );
        assert!(matches!(
            d.phase(),
            Phase::Establishing {
                anchor: Anchor::Checkpoint {
                    height: 20000,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn mn_off_establishes_before_header_sync() {
        let mut d = driver(false, None);
        let out = d.advance(Input::Opened(None)).unwrap();
        assert!(matches!(
            &out[..],
            [
                Output::Reset(_),
                Output::BuildChain(_),
                Output::EstablishUnguarded,
                Output::StartSubscribers,
                Output::SyncHeaders
            ]
        ));
        assert_eq!(d.phase(), &Phase::Established);
    }

    #[test]
    fn stale_resumes_coverage_and_failed_stops() {
        let mut d = driver(true, None);
        open(&mut d);
        d.advance(caught_up(8, 1)).unwrap();
        d.advance(coverage(Coverage::Covered, 1, 1)).unwrap();
        let stale = EstablishOutcome::Stale(HeaderState::CaughtUp {
            tip: tip(9),
            tip_gen: 2,
        });
        assert_eq!(d.advance(result(1, 1, stale)).unwrap(), vec![run(9, 2, 1)]);
        assert!(matches!(d.phase(), Phase::Coverage { .. }));
        assert_eq!(
            d.advance(coverage(Coverage::Covered, 2, 1)).unwrap(),
            vec![establish(2, 9, 2, 1)]
        );
        assert_eq!(
            d.advance(result(2, 1, EstablishOutcome::Failed("disk".into()))).unwrap(),
            vec![Output::FailStop("disk".into())]
        );
        assert_eq!(d.phase(), &Phase::Failed);
        for input in [
            caught_up(10, 3),
            coverage(Coverage::Covered, 3, 1),
            Input::Replaced {
                anchor: Anchor::Genesis,
            },
        ] {
            assert!(d.advance(input).unwrap().is_empty());
        }
        assert_eq!(d.phase(), &Phase::Failed);
    }

    #[test]
    fn replaced_makes_a_new_candidate_without_a_reset() {
        let mut d = driver(true, None);
        open(&mut d);
        d.advance(caught_up(8, 1)).unwrap();
        let out = d
            .advance(Input::Replaced {
                anchor: Anchor::Genesis,
            })
            .unwrap();
        assert!(matches!(&out[..], [Output::Diagnostic(_)]), "{out:?}");
        assert_eq!(
            d.phase(),
            &Phase::Candidate {
                anchor: Anchor::Genesis
            }
        );
        assert_eq!(d.cand_gen(), 2);
        assert!(d.advance(coverage(Coverage::Covered, 1, 1)).unwrap().is_empty());
        assert!(d.advance(caught_up(8, 1)).unwrap().is_empty());
        assert_eq!(d.advance(caught_up(3, 2)).unwrap(), vec![run(3, 2, 2)]);
        assert_eq!(
            d.advance(coverage(Coverage::Covered, 2, 2)).unwrap(),
            vec![establish(1, 3, 2, 2)]
        );
    }

    /// Covered on tip 8 (`tip_gen` 1, `cand_gen` 1): `Establish { id: 1 }` is outstanding.
    fn establishing() -> InitialSync {
        let mut d = driver(true, None);
        open(&mut d);
        d.advance(caught_up(8, 1)).unwrap();
        assert_eq!(
            d.advance(coverage(Coverage::Covered, 1, 1)).unwrap(),
            vec![establish(1, 8, 1, 1)]
        );
        d
    }

    fn stale_on(n: u8, tip_gen: u64) -> EstablishOutcome {
        EstablishOutcome::Stale(HeaderState::CaughtUp {
            tip: tip(n),
            tip_gen,
        })
    }

    fn established_once(d: &mut InitialSync) {
        assert_eq!(d.phase(), &Phase::Established);
        for input in
            [caught_up(9, 2), caught_up(10, 3), coverage(Coverage::Covered, 3, 1), spent(20, 3, 1)]
        {
            assert!(d.advance(input).unwrap().is_empty());
        }
        let out = d.advance(result(1, 1, EstablishOutcome::Established)).unwrap();
        assert!(matches!(&out[..], [Output::Diagnostic(_)]), "{out:?}");
        assert_eq!(d.phase(), &Phase::Established);
    }

    #[test]
    fn covered_establishes_only_on_the_hosts_result() {
        let mut d = establishing();
        assert!(matches!(
            d.phase(),
            Phase::Establishing {
                id: 1,
                tip_gen: 1,
                recorded: None,
                replaced: None,
                ..
            }
        ));
        for input in [
            coverage(Coverage::Covered, 1, 1),
            coverage(
                Coverage::Below {
                    lowest: None,
                },
                1,
                1,
            ),
            spent(20, 1, 1),
        ] {
            assert!(d.advance(input).unwrap().is_empty());
        }
        assert!(d.advance(Input::Opened(None)).is_err());
        assert_eq!(d.cand_gen(), 1);
        assert_eq!(d.coverage_rounds_spent(), 0);
        assert!(matches!(
            d.phase(),
            Phase::Establishing {
                id: 1,
                ..
            }
        ));
    }

    #[test]
    fn established_after_recorded_publication() {
        let mut d = establishing();
        assert!(d.advance(caught_up(9, 2)).unwrap().is_empty());
        assert!(matches!(
            d.phase(),
            Phase::Establishing {
                recorded: Some(HeaderState::CaughtUp {
                    tip_gen: 2,
                    ..
                }),
                ..
            }
        ));
        assert_eq!(
            d.advance(result(1, 1, EstablishOutcome::Established)).unwrap(),
            vec![Output::StartSubscribers]
        );
        established_once(&mut d);
    }

    #[test]
    fn established_then_publication() {
        let mut d = establishing();
        assert_eq!(
            d.advance(result(1, 1, EstablishOutcome::Established)).unwrap(),
            vec![Output::StartSubscribers]
        );
        established_once(&mut d);
    }

    #[test]
    fn behind_recorded_before_established_is_discarded() {
        let mut d = establishing();
        assert!(d
            .advance(Input::HeadersBehind {
                tip_gen: 1
            })
            .unwrap()
            .is_empty());
        assert_eq!(
            d.advance(result(1, 1, EstablishOutcome::Established)).unwrap(),
            vec![Output::StartSubscribers]
        );
        established_once(&mut d);
    }

    #[test]
    fn stale_after_recorded_publication() {
        let mut d = establishing();
        d.advance(caught_up(9, 2)).unwrap();
        assert_eq!(d.advance(result(1, 1, stale_on(9, 2))).unwrap(), vec![run(9, 2, 1)]);
        assert!(d.advance(caught_up(9, 2)).unwrap().is_empty());
        assert_eq!(
            d.advance(coverage(Coverage::Covered, 2, 1)).unwrap(),
            vec![establish(2, 9, 2, 1)]
        );

        let mut d = establishing();
        assert_eq!(d.advance(result(1, 1, stale_on(9, 2))).unwrap(), vec![run(9, 2, 1)]);
        assert!(d.advance(caught_up(9, 2)).unwrap().is_empty());
        assert_eq!(d.advance(caught_up(10, 3)).unwrap(), vec![run(10, 3, 1)]);
    }

    #[test]
    fn stale_runs_coverage_on_the_newer_state() {
        let mut d = establishing();
        d.advance(caught_up(9, 2)).unwrap();
        assert_eq!(d.advance(result(1, 1, stale_on(8, 1))).unwrap(), vec![run(9, 2, 1)]);

        let mut d = establishing();
        d.advance(caught_up(9, 2)).unwrap();
        d.advance(caught_up(10, 3)).unwrap();
        assert!(d.advance(caught_up(9, 2)).unwrap().is_empty());
        assert_eq!(d.advance(result(1, 1, stale_on(9, 2))).unwrap(), vec![run(10, 3, 1)]);

        let mut d = establishing();
        d.advance(caught_up(9, 2)).unwrap();
        let behind = EstablishOutcome::Stale(HeaderState::Behind {
            tip_gen: 3,
        });
        assert!(d.advance(result(1, 1, behind)).unwrap().is_empty());
        assert!(matches!(d.phase(), Phase::Coverage { .. }));
        assert!(d.advance(coverage(Coverage::Covered, 3, 1)).unwrap().is_empty());
        assert_eq!(d.advance(caught_up(11, 3)).unwrap(), vec![run(11, 3, 1)]);
        assert_eq!(
            d.advance(coverage(Coverage::Covered, 3, 1)).unwrap(),
            vec![establish(2, 11, 3, 1)]
        );
    }

    #[test]
    fn failed_after_recorded_publication() {
        for publish_first in [true, false] {
            let mut d = establishing();
            if publish_first {
                d.advance(caught_up(9, 2)).unwrap();
            }
            assert_eq!(
                d.advance(result(1, 1, EstablishOutcome::Failed("stopped".into()))).unwrap(),
                vec![Output::FailStop("stopped".into())]
            );
            assert_eq!(d.phase(), &Phase::Failed);
            assert!(d.advance(caught_up(10, 3)).unwrap().is_empty());
            let out = d.advance(result(1, 1, EstablishOutcome::Established)).unwrap();
            assert!(matches!(&out[..], [Output::Diagnostic(_)]), "{out:?}");
            assert_eq!(d.phase(), &Phase::Failed);
        }
    }

    #[test]
    fn result_with_wrong_id_or_cand_gen_is_dropped() {
        let mut d = establishing();
        for (id, cand_gen) in [(2, 1), (0, 1), (1, 0), (1, 2)] {
            for outcome in [
                EstablishOutcome::Established,
                stale_on(9, 2),
                EstablishOutcome::Failed("x".into()),
            ] {
                let out = d.advance(result(id, cand_gen, outcome)).unwrap();
                assert!(matches!(&out[..], [Output::Diagnostic(_)]), "{out:?}");
                assert!(matches!(
                    d.phase(),
                    Phase::Establishing {
                        id: 1,
                        ..
                    }
                ));
            }
        }
        assert_eq!(
            d.advance(result(1, 1, EstablishOutcome::Established)).unwrap(),
            vec![Output::StartSubscribers]
        );
    }

    #[test]
    fn replaced_while_establishing() {
        let replacement = Anchor::Genesis;
        for outcome in [EstablishOutcome::Established, stale_on(9, 2)] {
            let mut d = establishing();
            assert!(d
                .advance(Input::Replaced {
                    anchor: replacement.clone()
                })
                .unwrap()
                .is_empty());
            d.advance(caught_up(9, 2)).unwrap();
            assert!(matches!(
                d.phase(),
                Phase::Establishing {
                    id: 1,
                    replaced: Some(Anchor::Genesis),
                    ..
                }
            ));
            assert_eq!(d.cand_gen(), 1);
            let out = d.advance(result(1, 1, outcome)).unwrap();
            assert!(matches!(&out[..], [Output::Diagnostic(_)]), "{out:?}");
            assert_eq!(
                d.phase(),
                &Phase::Candidate {
                    anchor: replacement.clone()
                }
            );
            assert_eq!(d.cand_gen(), 2);
            assert!(d.advance(caught_up(9, 2)).unwrap().is_empty());
            assert!(d.advance(coverage(Coverage::Covered, 2, 1)).unwrap().is_empty());
            assert_eq!(d.advance(caught_up(12, 4)).unwrap(), vec![run(12, 4, 2)]);
            assert_eq!(
                d.advance(coverage(Coverage::Covered, 4, 2)).unwrap(),
                vec![establish(2, 12, 4, 2)]
            );
        }
        let mut d = establishing();
        d.advance(Input::Replaced {
            anchor: replacement,
        })
        .unwrap();
        assert_eq!(
            d.advance(result(1, 1, EstablishOutcome::Failed("disk".into()))).unwrap(),
            vec![Output::FailStop("disk".into())]
        );
        assert_eq!(d.phase(), &Phase::Failed);
    }

    #[test]
    fn a_reorg_during_coverage_never_establishes_the_older_branch() {
        let mut d = driver(true, None);
        open(&mut d);
        d.advance(caught_up(8, 1)).unwrap();
        d.advance(caught_up(108, 2)).unwrap();
        assert!(d.advance(coverage(Coverage::Covered, 1, 1)).unwrap().is_empty());
        assert!(d.advance(spent(20, 1, 1)).unwrap().is_empty());
        assert!(d
            .advance(coverage(
                Coverage::Below {
                    lowest: None
                },
                1,
                1
            ))
            .unwrap()
            .is_empty());
        assert_eq!(d.coverage_rounds_spent(), 0);
        assert_eq!(
            d.advance(coverage(Coverage::Covered, 2, 1)).unwrap(),
            vec![establish(1, 108, 2, 1)]
        );
    }

    #[test]
    fn stale_results_after_a_fallback_are_dropped() {
        let mut d = driver(true, None);
        open(&mut d);
        d.advance(caught_up(8, 1)).unwrap();
        d.advance(coverage(
            Coverage::Below {
                lowest: None,
            },
            1,
            1,
        ))
        .unwrap();
        d.advance(caught_up(5, 2)).unwrap();
        for stale in [
            coverage(Coverage::Covered, 2, 1),
            coverage(
                Coverage::Below {
                    lowest: None,
                },
                2,
                1,
            ),
            spent(20, 2, 1),
        ] {
            assert!(d.advance(stale).unwrap().is_empty());
        }
        let out = d.advance(result(1, 1, EstablishOutcome::Established)).unwrap();
        assert!(matches!(&out[..], [Output::Diagnostic(_)]), "{out:?}");
        assert_eq!(d.coverage_rounds_spent(), 0);
        assert!(matches!(
            d.phase(),
            Phase::Coverage {
                anchor: Anchor::Checkpoint {
                    height: 10000,
                    ..
                }
            }
        ));
    }

    #[test]
    fn pending_and_spent_rounds_never_fall_back() {
        let mut d = driver(true, None);
        let anchor = open(&mut d);
        d.advance(caught_up(8, 1)).unwrap();
        for _ in 0..50 {
            assert!(d.advance(coverage(Coverage::Pending, 1, 1)).unwrap().is_empty());
            let out = d.advance(spent(20, 1, 1)).unwrap();
            assert!(out.iter().all(|o| matches!(o, Output::Diagnostic(_))), "{out:?}");
        }
        assert_eq!(
            d.phase(),
            &Phase::Coverage {
                anchor
            }
        );
        assert_eq!(d.cand_gen(), 1);
    }

    fn established_at(anchor: &Anchor, llmq: &LlmqConfig) -> ChainMeta {
        meta(anchor, AnchorState::Established, llmq)
    }

    #[test]
    fn established_opens_from_its_own_window_without_a_pack() {
        let with_packs = driver(true, None);
        let stored = pack_anchor(&with_packs.packs[1]);
        let off_pack = pack_anchor(&pack(25000, 1_500_000));
        for (mut d, anchor) in [
            (configured_driver(true, None, None, false), stored.clone()),
            (driver(true, None), off_pack.clone()),
            (driver(false, None), off_pack),
        ] {
            let out = d.advance(reopen(established_at(&anchor, &d.llmq), &anchor)).unwrap();
            assert_eq!(
                out,
                vec![Output::BuildChain(anchor), Output::StartSubscribers, Output::SyncHeaders]
            );
            assert_eq!(d.phase(), &Phase::Established);
            assert_eq!(d.cand_gen(), 0);
        }
    }

    #[test]
    fn a_corrupted_window_body_under_an_intact_hash_fails_the_open() {
        let d = configured_driver(true, None, None, false);
        let anchor = pack_anchor(&pack(20000, 1_000_000));
        for k in [0, 10, 24] {
            let mut records = window_records(&anchor);
            let intact = records[k].hash;
            let nonce = records[k].header.nonce() ^ 1;
            records[k].header.set_nonce(nonce);
            let err = read(established_at(&anchor, &d.llmq), records).unwrap_err();
            assert!(
                matches!(
                    err,
                    StoredAnchorError::Rehash { height, stored, .. }
                        if height == 19976 + k as u32 && stored == intact
                ),
                "{err:?}"
            );
        }
    }

    #[test]
    fn a_window_that_does_not_lead_to_the_anchor_fails_the_open() {
        let d = configured_driver(true, None, None, false);
        let anchor = pack_anchor(&pack(20000, 1_000_000));
        let meta = established_at(&anchor, &d.llmq);
        let other = window_records(&pack_anchor(&pack(20000, 1_000_001)));

        let mut moved = meta;
        moved.anchor.0 = other[24].hash;
        let err = read(moved, window_records(&anchor)).unwrap_err();
        assert!(
            matches!(
                err,
                StoredAnchorError::MissingAnchor {
                    height: 20000,
                    ..
                }
            ),
            "{err:?}"
        );

        let mut swapped = window_records(&anchor);
        swapped[24] = other[24].clone();
        swapped[24].hash = anchor_hash(&anchor);
        let err = read(meta, swapped).unwrap_err();
        assert!(
            matches!(
                err,
                StoredAnchorError::Rehash {
                    height: 20000,
                    ..
                }
            ),
            "{err:?}"
        );

        let mut moved = meta;
        moved.anchor.1 = 20001;
        let err = read(moved, window_records(&anchor)).unwrap_err();
        assert!(
            matches!(
                err,
                StoredAnchorError::Broken {
                    height: 20000
                }
            ),
            "{err:?}"
        );

        let mut relinked = window_records(&anchor);
        relinked[10] = other[10].clone();
        relinked[10].hash = relinked[11].header.prev_hash();
        let err = read(meta, relinked).unwrap_err();
        assert!(
            matches!(
                err,
                StoredAnchorError::Rehash {
                    height: 19986,
                    ..
                }
            ),
            "{err:?}"
        );

        for gone in [0, 12, 22] {
            let mut records = window_records(&anchor);
            records.remove(gone);
            let err = read(meta, records).unwrap_err();
            assert!(
                matches!(err, StoredAnchorError::Broken { height } if height == 19976 + gone as u32),
                "{err:?}"
            );
        }
        let err = read(meta, Vec::new()).unwrap_err();
        assert!(matches!(err, StoredAnchorError::MissingAnchor { .. }), "{err:?}");
    }

    #[test]
    fn a_legacy_anchor_without_a_window_fails_the_open() {
        let mut d = configured_driver(true, None, None, false);
        let anchor = pack_anchor(&pack(25000, 1_500_000));
        let stored = read(established_at(&anchor, &d.llmq), window_records(&anchor)[24..].to_vec());
        let err = d.advance(Input::Opened(Some(stored.unwrap()))).unwrap_err();
        assert!(
            matches!(
                err,
                SpvError::StoredAnchor(StoredAnchorError::LegacyAnchor {
                    height: 25000
                })
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("wipe the storage and resync"), "{err}");
        assert_eq!(d.phase(), &Phase::Unopened);
    }
}

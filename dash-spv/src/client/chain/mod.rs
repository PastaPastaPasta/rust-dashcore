//! The header chain over the SPV store: opened, created, reset from a legacy store, and
//! replaced, with its source published by generation.

mod ingress;
mod no_wait;
mod pinned;
mod score;
mod step;
mod store;
mod sub;
mod task;
#[cfg(test)]
pub(crate) mod tests;

use std::sync::Arc;

use dash_chain::{
    Anchor, AnchorPack, AnchorState, ChainConfig, ChainError, ChainFeeds, ChainReader, Durability,
    Evicting, FeedConfig, HeaderChain, OpenError, Services, StaticSporks, StoreError,
};
use dash_mnlist::LlmqConfig;
use tokio::sync::watch;

use super::anchor::{candidate_meta, InitialSync, Input, Output, StoredChain};
use super::{ClientConfig, SpvFailStop};
use crate::chain::checkpoints::{Checkpoint, CheckpointManager};
use crate::error::{SpvError, StorageError, SyncError};
use crate::storage::store as spv_store;
use crate::storage::SpvStore;

pub use ingress::{HeaderIngress, IngressReply, Offered, INGRESS_CAP};
pub use no_wait::{SpvJoin, SpvReply, SubError};
use pinned::GenGuard;
pub use pinned::PinnedChain;
pub use score::{HeaderMisbehaviour, Refused};
pub use step::LagBudget;
pub use store::{SpvChainStore, StoreHandle};
pub use sub::{ChainNotice, ChainSub, ResyncWhy, SubItem, SubKind};
pub use task::{ClReply, HeaderHandle, HeadersReply, IngressReplies, ReplaceOutcome};

/// The resident window of the SPV header chain.
pub const HEADER_WINDOW: u32 = 10_000;

/// The header chain's generation: 1 for the chain the host opened, one more at each
/// replacement. In memory only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChainGen(pub u64);

/// The chain a subscriber reads, by generation. The only way a subscriber learns of a chain.
#[derive(Clone)]
pub struct ChainSource {
    pub gen: ChainGen,
    pub state: SourceState,
    guard: Arc<GenGuard>,
}

#[derive(Clone)]
pub enum SourceState {
    Live {
        feeds: ChainFeeds,
        reader: ChainReader,
    },
    /// The chain of `gen` is being dropped and its store reset.
    Replacing,
    Stopped,
}

impl ChainSource {
    fn live<H: StoreHandle>(gen: ChainGen, chain: &Chain<H>, guard: &Arc<GenGuard>) -> Self {
        Self {
            gen,
            state: SourceState::Live {
                feeds: chain.feeds(),
                reader: chain.reader(),
            },
            guard: Arc::clone(guard),
        }
    }

    fn with(&self, state: SourceState) -> Self {
        Self {
            gen: self.gen,
            state,
            guard: Arc::clone(&self.guard),
        }
    }
}

/// What the host opens and builds chains with.
#[derive(Clone)]
pub struct ChainHostSetup {
    pub config: ClientConfig,
    pub llmq: Arc<LlmqConfig>,
    pub packs: Vec<AnchorPack>,
    /// The wallets' birth height, as the client resolves it.
    pub wallet_birth: Option<u32>,
    pub fail: Arc<SpvFailStop>,
    /// SPORK_19 as the client sees it: the one instance every chain the host builds reads,
    /// shared with the client's `ChainLockManager` (PLAN-r1 §2.2.4).
    pub sporks: Arc<StaticSporks>,
    /// The chain's feeds: SPV's defaults.
    pub feeds: FeedConfig,
    /// The resident window: default [`HEADER_WINDOW`]; tests shrink it.
    pub window: u32,
    /// How long a header step waits for its subscribers: SPV's defaults.
    pub lag: LagBudget,
}

impl ChainHostSetup {
    /// The setup with the network's embedded anchor packs and the client's spork instance.
    pub fn new(
        config: ClientConfig,
        llmq: Arc<LlmqConfig>,
        wallet_birth: Option<u32>,
        fail: Arc<SpvFailStop>,
        sporks: Arc<StaticSporks>,
    ) -> Self {
        let packs = CheckpointManager::for_network(config.network)
            .checkpoints()
            .filter_map(Checkpoint::anchor_pack)
            .collect();
        Self {
            config,
            llmq,
            packs,
            wallet_birth,
            fail,
            sporks,
            feeds: FeedConfig::spv(),
            window: HEADER_WINDOW,
            lag: LagBudget::spv(&FeedConfig::spv()),
        }
    }

    fn services(&self) -> Services {
        Services {
            clock: Arc::clone(&self.config.clock),
            sporks: self.sporks.clone(),
            fail: self.fail.clone(),
        }
    }

    /// `r`, with a failure tripping the fail-stop: the store's state after it is unknown.
    fn durably<T>(&self, r: spv_store::Result<T>, what: &str) -> Result<T, SpvError> {
        r.map_err(|e| {
            self.fail.trip(format!("{what}: {e}"));
            SpvError::Storage(e.into())
        })
    }
}

pub type Chain<H> = HeaderChain<SpvChainStore<H>, Evicting>;

/// Owns the header task's handles and the initial sync's driver.
pub struct SpvChainHost {
    handle: HeaderHandle,
    source: watch::Receiver<ChainSource>,
    subs: Arc<sub::SubTable>,
    ingress: Option<(HeaderIngress, IngressReplies)>,
    sync: InitialSync,
}

impl SpvChainHost {
    /// Opens the store's chain, or creates one (`open_chain`), in one store job, and starts
    /// the header task on it, `Live` at generation 1.
    pub async fn open(store: SpvStore, setup: ChainHostSetup) -> Result<Self, SpvError> {
        Self::open_on(store, setup).await
    }

    pub(crate) async fn open_on<H: StoreHandle>(
        h: H,
        setup: ChainHostSetup,
    ) -> Result<Self, SpvError> {
        let job = {
            let (h, setup) = (h.clone(), setup.clone());
            move || open_chain(&h, &setup)
        };
        let (chain, sync) = h.job(job).await??;
        let subs = sub::SubTable::new();
        let (handle, source, ingress, replies) = task::spawn(h, setup, chain, Arc::clone(&subs));
        Ok(Self {
            handle,
            source,
            subs,
            ingress: Some((ingress, replies)),
            sync,
        })
    }

    pub fn handle(&self) -> HeaderHandle {
        self.handle.clone()
    }

    /// The task's ingress and its replies, for the headers manager: `None` once taken.
    pub fn take_ingress(&mut self) -> Option<(HeaderIngress, IngressReplies)> {
        self.ingress.take()
    }

    pub fn source(&self) -> watch::Receiver<ChainSource> {
        self.source.clone()
    }

    /// A subscriber of the chain, across its generations (PLAN-r1 §2.4).
    pub fn subscribe(&self, kind: SubKind) -> ChainSub {
        ChainSub::new(kind, self.source(), Arc::clone(&self.subs))
    }

    /// The chain as its latest publication left it; `None` while it is being replaced or once
    /// the task has stopped.
    pub fn pinned(&self) -> Option<PinnedChain> {
        let source = self.source.borrow();
        match &source.state {
            SourceState::Live {
                reader,
                ..
            } => Some(PinnedChain::new(reader, source.gen, Arc::clone(&source.guard))),
            SourceState::Replacing | SourceState::Stopped => None,
        }
    }

    pub fn initial_sync(&self) -> &InitialSync {
        &self.sync
    }

    /// Closes every subscriber's feed, so a header step parked on a pump returns: a stop calls
    /// it before it drains the store. A subscriber still in `next()` resyncs.
    pub fn stop_subscribers(&self) {
        self.subs.close_all("stop");
    }
}

/// The store's chain, as the initial sync decides it:
/// - a stored `meta` opens it (a Candidate whose reset is not yet followed by the anchor's
///   first step is created again);
/// - a fresh store, a legacy one (headers no reset wrote) and a Candidate whose pack is gone
///   get one Durable reset to a candidate anchor, then a new chain and its first step; a
///   legacy store's candidate is born at most at the store's start, unless
///   `start_from_height` names a start. The reset clears every subscriber's rows and
///   locator, so each subscriber starts again at the anchor; recovery follows it;
/// - with masternode sync off, the new chain is established at once.
///
/// Every read re-hashes the records it returns; a failed write trips the fail-stop.
pub(crate) fn open_chain<H: StoreHandle>(
    h: &H,
    setup: &ChainHostSetup,
) -> Result<(Chain<H>, InitialSync), SpvError> {
    let store = h.store();
    let legacy = store.legacy_start();
    let birth = match legacy {
        Some(start) => Some(setup.wallet_birth.map_or(start, |b| b.min(start))),
        None => setup.wallet_birth,
    };
    let mut sync =
        InitialSync::new(setup.config.clone(), Arc::clone(&setup.llmq), setup.packs.clone(), birth);
    let stored = match legacy {
        Some(_) => None,
        None => StoredChain::read(store)?,
    };
    let established =
        stored.as_ref().is_some_and(|s| s.meta().anchor_state == AnchorState::Established);
    let mut chain = None;
    for out in sync.advance(Input::Opened(stored))? {
        match out {
            Output::Diagnostic(d) => tracing::warn!("{d}"),
            Output::Reset(meta) => {
                let done = match legacy {
                    Some(_) => store.reset_legacy(meta),
                    None => store.reset(meta),
                };
                setup.durably(done.and_then(|()| store.recover()), "resetting the header store")?;
            }
            Output::BuildChain(anchor) => chain = Some(build(h, setup, &anchor, established)?),
            Output::EstablishUnguarded => {
                setup.durably(store.establish(), "establishing the header chain")?
            }
            Output::StartSubscribers | Output::SyncHeaders => {}
            other => return Err(SyncError::InvalidState(format!("{other:?} at open")).into()),
        }
    }
    let chain = chain.ok_or_else(|| SyncError::InvalidState("no chain at open".into()))?;
    Ok((chain, sync))
}

/// Replacement's store job: the old generation `retired` once its pinned reads in flight are
/// done, one Durable reset to a Candidate at `anchor` (refused on an Established store), then
/// the new chain and its first step.
fn replace_chain<H: StoreHandle>(
    h: &H,
    setup: &ChainHostSetup,
    anchor: &Anchor,
    retired: &GenGuard,
) -> Result<Chain<H>, SpvError> {
    retired.retire();
    let meta = candidate_meta(setup.llmq.params(), anchor);
    setup.durably(h.store().reset(meta), "resetting the header store")?;
    build(h, setup, anchor, false)
}

/// The chain at `anchor`: opened when the store holds one, else new with its first step.
fn build<H: StoreHandle>(
    h: &H,
    setup: &ChainHostSetup,
    anchor: &Anchor,
    established: bool,
) -> Result<Chain<H>, SpvError> {
    let params = Arc::clone(setup.llmq.params_arc());
    let mut cfg = ChainConfig::spv(&params, setup.window);
    cfg.feeds = setup.feeds;
    let store = SpvChainStore(h.clone());
    if established || h.store().view().active.hi.is_some() {
        return HeaderChain::open(params, cfg, setup.services(), store).map_err(open_error);
    }
    let mut chain = match anchor_pack(anchor) {
        None => HeaderChain::new_genesis(params, cfg, setup.services(), store),
        Some(pack) => HeaderChain::new_anchored(params, cfg, setup.services(), store, &pack),
    }
    .map_err(chain_error)?;
    let (tip, _) =
        chain.private_tip().ok_or_else(|| SyncError::InvalidState("a new chain's tip".into()))?;
    let mut step = setup.durably(h.store().begin_step(), "the anchor's header step")?;
    let tail = chain.write_header_step(None, &mut step).map_err(chain_error)?;
    setup.durably(step.commit(&tail, Durability::Durable), "the anchor's header step")?;
    chain.activate_segmented(tip, &mut || {}).map_err(chain_error)?;
    chain.records_committed();
    Ok(chain)
}

fn anchor_pack(anchor: &Anchor) -> Option<AnchorPack> {
    match anchor {
        Anchor::Genesis => None,
        Anchor::Checkpoint {
            height,
            hash,
            chain_work,
            window,
        } => Some(AnchorPack {
            height: *height,
            hash: *hash,
            chain_work: *chain_work,
            headers: **window,
        }),
    }
}

fn store_error(e: StoreError) -> SpvError {
    match e {
        StoreError::Corrupt(what) => StorageError::Corruption(what),
        e => StorageError::ReadFailed(e.to_string()),
    }
    .into()
}

fn chain_error(e: ChainError) -> SpvError {
    match e {
        ChainError::Storage(e) => store_error(e),
        e => SyncError::InvalidState(format!("header chain: {e}")).into(),
    }
}

fn open_error(e: OpenError) -> SpvError {
    match e {
        OpenError::Storage(e) => store_error(e),
        OpenError::Corrupt(c) => StorageError::Corruption(c.to_string()).into(),
        OpenError::Config(e) => SpvError::Config(e.to_string()),
        e => SyncError::InvalidState(format!("opening the header chain: {e}")).into(),
    }
}

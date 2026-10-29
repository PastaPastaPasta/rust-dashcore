//! LQ1's and SB8's views over the engine's state (EV addendum §2.2–§2.3): the chain, deployments
//! by height, the held lists and coinbases, the `qrinfo` rows, and the scan the lock checks read.
//!
//! Every view obeys the not-held rule (addendum §2.1): an input the engine does not hold is
//! `Ok(None)` from [`LlmqSource`] (LQ1 turns it into `Missing`) or an [`MnViewError`], never one of
//! Core's absence values.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use dash_chain::AncestryError;
use dash_consensus::BlockId;
use dash_consensus::ctx::{Deployment, DeploymentView, HeaderCtx};
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::llmq::{
    LlmqChain, LlmqCtx, LlmqSource, MemberError, QuorumSnapshot, SnapshotRows,
    is_quorum_rotation_enabled,
};
use dash_evo::sml::SmlList;
use dash_params::ChainParams;
use dash_primitives::{BlockHash, LlmqType};
use dash_types::Numeric;

use crate::chain::MnChain;
use crate::engine::HeldList;
use crate::error::MnViewError;

/// The chain as LQ1 reads it ([`LlmqChain`]): [`MnChain::ancestry`] with SPV's error.
pub(crate) struct ChainView<'a> {
    pub chain: &'a dyn MnChain,
}

impl LlmqChain for ChainView<'_> {
    type Error = MnViewError;

    fn header(&self, id: BlockId) -> Result<HeaderCtx, MnViewError> {
        Ok(self.chain.ancestry().header(id)?)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, MnViewError> {
        Ok(self.chain.ancestry().ancestor(id, height)?)
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, MnViewError> {
        Ok(self.chain.ancestry().prev(id)?)
    }
}

/// Deployments by height (D6): a buried deployment is active from its chain-parameter height; no
/// versionbits deployment is active (SPV's used types read only buried gates).
///
/// [`DeploymentView`] cannot fail, so a failed header read is latched here, answers `false`, and
/// the caller takes it with [`Heights::take_error`] after the call and discards the call's result.
pub(crate) struct Heights<'a> {
    params: &'a ChainParams,
    chain: &'a dyn MnChain,
    failed: Mutex<Option<MnViewError>>,
}

impl<'a> Heights<'a> {
    pub fn new(params: &'a ChainParams, chain: &'a dyn MnChain) -> Self {
        Heights {
            params,
            chain,
            failed: Mutex::new(None),
        }
    }

    /// The first failed read since the last call, if any.
    pub fn take_error(&self) -> Option<MnViewError> {
        self.failed.lock().expect("not poisoned").take()
    }

    fn height(&self, id: BlockId) -> u32 {
        match self.chain.ancestry().header(id) {
            Ok(h) => h.height,
            Err(e) => {
                self.failed.lock().expect("not poisoned").get_or_insert(e.into());
                0
            }
        }
    }

    fn active_from(&self, height: u32, deployment: Deployment) -> bool {
        match deployment {
            Deployment::Buried(d) => height >= self.params.consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(_) => false,
        }
    }
}

impl DeploymentView for Heights<'_> {
    // Core: deploymentstatus.h DeploymentActiveAfter (buried: nHeight + 1 >= height)
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| self.height(p).saturating_add(1));
        self.active_from(next, deployment)
    }

    // Core: deploymentstatus.h DeploymentActiveAt (buried: nHeight >= height)
    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_from(self.height(block), deployment)
    }
}

/// The held lists and coinbases as LQ1's [`LlmqSource`]: `Ok(None)` for a block the engine does
/// not hold whole, a set-only height included (D28; LQ1's `Missing`, a request).
pub(crate) struct Source<'a> {
    pub lists: &'a BTreeMap<BlockId, HeldList>,
}

impl LlmqSource<SmlList> for Source<'_> {
    type Error = MnViewError;

    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<SmlList>>, MnViewError> {
        Ok(self.lists.get(&at).and_then(|l| l.full.as_ref()).map(|f| f.sml.clone()))
    }

    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, MnViewError> {
        Ok(self.lists.get(&at).and_then(|l| l.full.as_ref()).map(|f| f.coinbase.clone()))
    }
}

/// A rotation row's key: the type and the cycle base hash.
pub(crate) type RowKey = (u8, [u8; 32]);

/// A rotation row the engine holds (D20). No root commits to a row, so a reply's row is a
/// candidate until LQ1 computes the cycle's row itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HeldRow {
    /// Core's: LQ1 computed it from authenticated lists and Core's rows.
    Core(QuorumSnapshot),
    /// The distinct rows replies carried, in arrival order, at most `ROW_CANDIDATES`.
    Candidates(Vec<QuorumSnapshot>),
}

/// The rows as LQ1's [`SnapshotRows`] (D5, D20): Core's row where the engine confirmed one, else
/// the candidate `choice` names (the first by default), or with `candidates` unset none, so a
/// computation reads Core's rows only.
///
/// A cycle where rotation is not enabled has no row in Core: `GetAllQuorumMembers` stores one
/// only on the rotated path (`llmq/utils.cpp` rc.3:731-760), so that is Core's absence and LQ1's
/// rotation stops there. Any other row the engine does not hold is [`MnViewError::RowNotHeld`],
/// never Core's absent row (addendum §2.1). The row LQ1 stores, the cycle's own, is kept in
/// `stored`; LQ1 stores it last and never reads it back in the same call.
pub(crate) struct Rows<'a> {
    pub held: &'a HashMap<RowKey, HeldRow>,
    pub choice: &'a HashMap<RowKey, usize>,
    pub candidates: bool,
    pub ctx: LlmqCtx<'a, MnViewError>,
    pub chain: &'a dyn MnChain,
    /// The keys whose candidate was read, and how many candidates each has.
    pub read: RefCell<BTreeMap<RowKey, usize>>,
    pub stored: Option<(RowKey, QuorumSnapshot)>,
}

impl<'a> Rows<'a> {
    pub fn new(
        held: &'a HashMap<RowKey, HeldRow>,
        choice: &'a HashMap<RowKey, usize>,
        candidates: bool,
        ctx: LlmqCtx<'a, MnViewError>,
        chain: &'a dyn MnChain,
    ) -> Self {
        Rows {
            held,
            choice,
            candidates,
            ctx,
            chain,
            read: RefCell::new(BTreeMap::new()),
            stored: None,
        }
    }

    /// Whether rotation is not enabled at the cycle `cycle_base` (Core's `IsQuorumRotationEnabled`).
    fn before_rotation(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<bool, MnViewError> {
        let Some(id) = self.chain.lookup(cycle_base)? else {
            return Ok(false);
        };
        let enabled =
            self.ctx.llmq(llmq_type).and_then(|lp| is_quorum_rotation_enabled(&self.ctx, lp, id));
        match enabled {
            Ok(enabled) => Ok(!enabled),
            Err(MemberError::View(e)) => Err(e),
            Err(_) => Ok(false),
        }
    }
}

impl SnapshotRows for Rows<'_> {
    type Error = MnViewError;

    fn get(
        &self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, MnViewError> {
        // Core's absence first: a peer's row for such a cycle is not Core's.
        if self.before_rotation(llmq_type, cycle_base)? {
            return Ok(None);
        }
        let key = (llmq_type.to_base(), *cycle_base.as_bytes());
        match self.held.get(&key) {
            Some(HeldRow::Core(row)) => Ok(Some(row.clone())),
            Some(HeldRow::Candidates(rows)) if self.candidates => {
                self.read.borrow_mut().insert(key, rows.len());
                Ok(Some(rows[self.choice.get(&key).copied().unwrap_or(0)].clone()))
            }
            _ => Err(MnViewError::RowNotHeld {
                llmq_type: llmq_type.to_base(),
                cycle_base: *cycle_base,
            }),
        }
    }

    fn store(
        &mut self,
        llmq_type: LlmqType,
        cycle_base: &BlockHash,
        snapshot: &QuorumSnapshot,
    ) -> Result<(), MnViewError> {
        self.stored = Some(((llmq_type.to_base(), *cycle_base.as_bytes()), snapshot.clone()));
        Ok(())
    }
}

/// `chain.ancestry().ancestor(id, height)` for a height at or below `id`'s: `Ok(None)` there
/// breaks the ancestry's contract, read as below the floor.
pub(crate) fn ancestor_at(
    chain: &dyn MnChain,
    id: BlockId,
    height: u32,
) -> Result<BlockId, AncestryError> {
    chain.ancestry().ancestor(id, height)?.ok_or(AncestryError::BelowFloor {
        height,
    })
}

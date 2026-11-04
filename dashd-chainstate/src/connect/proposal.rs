//! `TestBlockValidity`'s dummy index past DIP3 (MN0-2b-1; CP OD-CP-33): what the attempt reads
//! about the block it checks, which Core gives a `CBlockIndex` it never inserts.

use std::sync::atomic::{AtomicBool, Ordering};

use dash_chain::{ChainReader, DUMMY_ID};
use dash_consensus::Block;
use dash_consensus::ctx::{BlockId, Deployment, DeploymentPos, DeploymentView, VersionbitsAnswer};
use dash_params::ChainSetup;

use super::evo::BlockAt;
use crate::chain::{ConnectCx, Fatal};

/// Core's `indexDummy` for one `TestBlockValidity` call: the block's place at
/// [`DUMMY_ID`], and the reader and deployments its attempt reads it through. The connector
/// builds it once per call (OD-CP-33), and drops it with the attempt.
// Core: validation.cpp:4932-4936 TestBlockValidity
pub(crate) struct DummyIndex<'a> {
    at: BlockAt,
    reader: ChainReader,
    deployments: ProposalDeployments<'a>,
}

impl<'a> DummyIndex<'a> {
    /// The dummy for `block` on `prev`: `None` if `prev` is not in `cx`'s reader.
    pub(crate) fn new(cx: &ConnectCx<'a>, block: &Block, prev: BlockId) -> Option<Self> {
        let parent = cx.reader.entry(prev)?;
        let at = BlockAt {
            hash: block.hash(),
            height: parent.height + 1,
            prev: Some(prev),
            id: Some(DUMMY_ID),
        };
        Some(Self {
            at,
            reader: cx.reader.with_dummy(at.hash, *block.header().raw(), prev)?,
            deployments: ProposalDeployments::new(cx.deployments, cx.params, prev, at.height),
        })
    }

    /// The block's place: its id is [`DUMMY_ID`].
    pub(crate) fn at(&self) -> BlockAt {
        self.at
    }

    /// `base` with the dummy's reader and deployments.
    pub(crate) fn cx<'s>(&'s self, base: &ConnectCx<'s>) -> ConnectCx<'s> {
        ConnectCx {
            reader: &self.reader,
            deployments: &self.deployments,
            ..*base
        }
    }

    /// See [`ProposalDeployments::take_fault`].
    pub(crate) fn take_fault(&self) -> Option<Fatal> {
        self.deployments.take_fault()
    }
}

/// The deployments a proposal's attempt reads: V's own, with the dummy answered as Core answers
/// `indexDummy`. `DeploymentActiveAt(indexDummy, dep)` is the parent's `DeploymentActiveAfter`,
/// both for a buried deployment (`nHeight` is the parent's + 1) and for a versionbits one (its
/// `pprev` is the parent).
///
/// A question about the dummy's child, `DeploymentActiveAfter(indexDummy, dep)`, Core asks only
/// of a buried deployment: step 12's V19 (rc.5 `evo/specialtxman.cpp:1043`), answered from
/// `nHeight` alone (`deploymentstatus.h:14-18`), and answered so here. The other
/// `DeploymentActiveAfter` sites a proposal reaches take the parent (rc.5
/// `validation.cpp:353-354`, `:4525-4591`, `:5919-5920`, `:6692-6830`). A versionbits question
/// about the child would go through the versionbits cache, which is keyed by block
/// (`versionbits.cpp:280-296`), and the dummy's id is reused by the next proposal at another
/// place, so the answer could be cached for the wrong block. Core never asks one: it is a broken
/// invariant, and the attempt ends in [`Fatal::assert`] ([`ProposalDeployments::take_fault`]).
pub(crate) struct ProposalDeployments<'a> {
    inner: &'a dyn DeploymentView,
    params: &'a ChainSetup,
    parent: BlockId,
    /// The dummy's `nHeight`.
    height: u32,
    asked_after_dummy: AtomicBool,
}

impl<'a> ProposalDeployments<'a> {
    pub(super) fn new(
        inner: &'a dyn DeploymentView,
        params: &'a ChainSetup,
        parent: BlockId,
        height: u32,
    ) -> Self {
        Self {
            inner,
            params,
            parent,
            height,
            asked_after_dummy: AtomicBool::new(false),
        }
    }

    fn asked_after_dummy(&self) {
        self.asked_after_dummy.store(true, Ordering::Relaxed);
    }

    /// The assert a question about the dummy's child raises, once the attempt is over.
    pub(super) fn take_fault(&self) -> Option<Fatal> {
        self.asked_after_dummy.swap(false, Ordering::Relaxed).then(|| {
            Fatal::assert(
                "TestBlockValidity: a deployment question about the proposal's child (the \
                 dummy index as pindexPrev)",
            )
        })
    }
}

impl DeploymentView for ProposalDeployments<'_> {
    // Core: deploymentstatus.h:14-31 DeploymentActiveAfter
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        if prev == Some(DUMMY_ID) {
            return match deployment {
                // Core: deploymentstatus.h:14-18, `pindexPrev->nHeight + 1 >= DeploymentHeight`
                Deployment::Buried(dep) => {
                    let at = self.params.consensus.deployment_height(dep).to_u32();
                    u64::from(self.height) + 1 >= u64::from(at)
                }
                Deployment::Versionbits(_) => {
                    self.asked_after_dummy();
                    false
                }
            };
        }
        self.inner.active_after(prev, deployment)
    }

    // Core: deploymentstatus.h:34-50 DeploymentActiveAt; validation.cpp:4932-4936 indexDummy
    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        if block == DUMMY_ID {
            return self.inner.active_after(Some(self.parent), deployment);
        }
        self.inner.active_at(block, deployment)
    }

    // Core: versionbits.cpp:280-284 State, :292-296 StateSinceHeight
    fn versionbits(&self, prev: Option<BlockId>, pos: DeploymentPos) -> Option<VersionbitsAnswer> {
        if prev == Some(DUMMY_ID) {
            self.asked_after_dummy();
            return None;
        }
        self.inner.versionbits(prev, pos)
    }
}

#[cfg(test)]
#[path = "proposal_tests.rs"]
mod tests;

//! `ChainCommand::TestBlockValidity` (CH3 §2.14; CH1-node §3.1): Core's `TestBlockValidity` on
//! the tip, under V's `cs_main` scope (class M), through the connector.
//!
//! GBT checks the parent first (`rpc/mining.cpp:717-720`): V answers a block whose parent is not
//! its tip with [`NotTested::NotBestPrev`] and never calls the connector with it, and answers
//! GBT's duplicate lookup (`rpc/mining.cpp:707-715`) under the same hold (CH3a D123).

use std::cell::RefCell;

use dash_chain::Status;
use dash_consensus::Block;

use super::actor::V;
use super::command::{BlockState, NotTested};
use super::index::entry;
use super::seams::{
    AttemptOrigin, ConnectCx, ConnectOutcome, Fatal, HeldChainLocks, HeldProposalChecks,
};

impl V {
    // Core: validation.cpp:4923-4974 TestBlockValidity, after rpc/mining.cpp:707-720's checks
    /// `TestBlockValidity(state, …, block, Tip(), check_pow, check_merkle)`: the connector's
    /// verdict as Core's `BlockValidationState`, or [`NotTested`]: GBT's duplicate answers when
    /// `lookup`, then a block whose parent is not the tip. A readiness refusal is
    /// `BlockState::Error` and V keeps serving (§2.17).
    ///
    /// # Errors
    ///
    /// The connector's storage fault (CH3 §2.1): V stops, as on the tip path.
    pub(super) fn test_block_validity(
        &mut self,
        block: &Block,
        check_pow: bool,
        check_merkle: bool,
        lookup: bool,
    ) -> Result<Result<BlockState, NotTested>, Fatal> {
        // rpc/mining.cpp:708-715, in Core's order (`IsValid` fails a failed block, chain.h:305).
        if lookup && let Some(id) = self.reader.lookup(&block.hash()) {
            let status = entry(&self.reader, id).status;
            return Ok(Err(if status.is_valid(Status::VALID_SCRIPTS) {
                NotTested::Duplicate
            } else if status.has(Status::FAILED_MASK) {
                NotTested::DuplicateInvalid
            } else {
                NotTested::DuplicateInconclusive
            }));
        }
        // rpc/mining.cpp:717-720
        let tip = match self.chain.private_tip() {
            Some((tip, _)) if tip.hash == block.header().prev_hash() => tip,
            _ => return Ok(Err(NotTested::NotBestPrev)),
        };
        // Core evaluates `IsInitialBlockDownload()` at v:2801; nothing moves it inside the
        // scope, so it is read as the attempt starts, as the tip path's is (CH3a D124).
        let is_ibd = self.is_ibd();
        let now = self.now_secs();
        let chain = RefCell::new(&mut self.chain);
        let best_header = chain.borrow().best_header();
        let deployments = self.deployments.view();
        let chainlocks = HeldChainLocks::new(&chain, self.cl.sigs.best_height());
        let checks = HeldProposalChecks::new(&chain, &*self.checks, now);
        let cx = ConnectCx {
            params: &self.params,
            reader: &self.reader,
            deployments: &deployments,
            best_header,
            min_chain_work: self.reader.min_chain_work(),
            is_ibd,
            chainlocks: &chainlocks,
            guard: self.guard,
            origin: AttemptOrigin::Proposal,
        };
        let out = self.connector.test_block_validity(
            &cx,
            &mut self.vcx,
            &checks,
            block,
            tip.id,
            check_pow,
            check_merkle,
        )?;
        Ok(Ok(match out {
            ConnectOutcome::Connected => BlockState::Valid,
            ConnectOutcome::Invalid(err) => BlockState::Invalid(err),
            ConnectOutcome::SystemError(msg) => BlockState::Error(msg),
        }))
    }
}

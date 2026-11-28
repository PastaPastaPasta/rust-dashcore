//! Core v24's `CQuorumBlockProcessor` on the network (QF1): its `ProcessMessage` extension for
//! `qfcommit`, and `AlreadyHave` and `ProcessGetData` for `MSG_QUORUM_FINAL_COMMITMENT`. Every
//! node registers it, as every Core node has its block processor.
//!
//! The pool reads take the facade's leaf lock alone, as Core's take `minableCommitmentsCs`
//! alone. The verdict needs V (route B), which no holder of a header cell may wait on (P3 R12):
//! the handler runs the read and the solicitation gate in the peer region, then waits on V as a
//! continuation outside it, and applies the verdict in the reopened region. V runs every check
//! of Core's `ProcessMessage` past the gate, `Verify` included, and `AddMineableCommitment`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use dash_consensus::primitives::{P2pCommitment, commitment_hash, write_commitment};
use dash_dml::blockprocessor::QcBlockError;
use dash_dml::blockprocessor::pool::QfcommitOutcome;
use dash_num::Hash256;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, OutMsg, PeerHandle};
use dash_p2p_core::{InvType, Inventory};
use dash_primitives::QuorumHash;
use dash_types::Numeric as _;
use dashd_chainstate::chain::{ChainHandle, OnVMembersError, no_wait_marked};
use dashd_chainstate::llmq::facade::MembersFacade;
use dashd_p2p::processing::peer::assert_no_header_cell;
use dashd_p2p::processing::registry::{Extension, MessageProcessingResult};
use dashd_p2p::processing::seams::Interrupted;
use dashd_p2p::processing::solicit::{GetDataResponse, UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE};
use dashd_p2p::{HandlerCx, HandlerResult, NetHandler, PeerLogic, PostRegion};

static COMMANDS: [CommandString; 1] = [CommandString::QFCOMMIT];

/// What V answered for one commitment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Core's verdict past the solicitation gate.
    Verdict(QfcommitOutcome),
    /// No verdict: a signature outside the BLS domain BS12 reproduces (SB8), or a block-level
    /// refusal `qfcommit_verdict` does not give. Dropped unscored.
    NoVerdict(String),
    /// A failed read, Core's thrown `dbwrapper_error`, or a configured type outside
    /// `LlmqCtx::new`'s domain, which Core's params never give.
    ReadFailed(String),
    /// V answered nothing: it has stopped, or the node has no LLMQ state.
    NoAnswer,
}

/// [`QfcommitChain::process`]'s answer, once V replies.
pub type AnswerWait = Pin<Box<dyn Future<Output = Answer> + Send>>;

/// The block processor's checks and pool, as the handler reads them.
pub trait QfcommitChain: Send + Sync {
    /// Sends [`OnVBinding::process_qfcommit`] to V. Called outside the peer region.
    ///
    /// [`OnVBinding::process_qfcommit`]: dashd_chainstate::chain::OnVBinding::process_qfcommit
    fn process(&self, qc: P2pCommitment, common_version: i32) -> AnswerWait;
    /// `HasMineableCommitment(hash)`.
    fn has(&self, hash: &[u8; 32]) -> bool;
    /// `GetMineableCommitmentByHash(hash)`.
    fn get(&self, hash: &[u8; 32]) -> Option<Arc<P2pCommitment>>;
}

/// The node's chain side: route B on V for the verdict, the facade's pool for the reads. A node
/// without the facade (no evo state) has an empty pool and gives no verdict, without asking V.
pub struct NodeQfcommit {
    pub v: ChainHandle,
    pub facade: Option<Arc<MembersFacade>>,
}

impl QfcommitChain for NodeQfcommit {
    fn process(&self, qc: P2pCommitment, common_version: i32) -> AnswerWait {
        if self.facade.is_none() {
            return Box::pin(std::future::ready(Answer::NoAnswer));
        }
        assert_no_header_cell();
        debug_assert!(!no_wait_marked(), "a qfcommit verdict waits on V");
        let reply =
            self.v.on_v(move |on_v, q| answer(q.bind(on_v).process_qfcommit(&qc, common_version)));
        Box::pin(async move { reply.await.unwrap_or(Answer::NoAnswer) })
    }

    fn has(&self, hash: &[u8; 32]) -> bool {
        self.facade.as_ref().is_some_and(|f| f.has_mineable_commitment(hash))
    }

    fn get(&self, hash: &[u8; 32]) -> Option<Arc<P2pCommitment>> {
        self.facade.as_ref()?.mineable_commitment(hash)
    }
}

/// V's answer for [`OnVBinding::process_qfcommit`]'s result: a store fault or a failed read
/// is a failed read, any other block-level refusal is no verdict.
///
/// [`OnVBinding::process_qfcommit`]: dashd_chainstate::chain::OnVBinding::process_qfcommit
pub(crate) fn answer(r: Result<Result<QfcommitOutcome, QcBlockError>, OnVMembersError>) -> Answer {
    match r {
        Ok(Ok(outcome)) => Answer::Verdict(outcome),
        Ok(Err(QcBlockError::Fault(e))) => Answer::ReadFailed(e.to_string()),
        Ok(Err(e)) => Answer::NoVerdict(e.to_string()),
        Err(e) => Answer::ReadFailed(e.to_string()),
    }
}

/// A failed read inside `ProcessMessage`: Core's `ProcessMessages` catches the store's
/// `dbwrapper_error`, logs it and keeps the peer unscored (DEC-1881).
// Core: v24.0.0 src/net_processing.cpp:5945-5962 ProcessMessages; src/dbwrapper.h:46
fn read_failed(error: &str) -> Throw {
    Throw {
        what: format!("CQuorumBlockProcessor: a qfcommit read failed: {error}").into(),
        type_name: "15dbwrapper_error",
    }
}

/// The node's block processor handler.
pub struct NetQuorumBlockProcessor(pub Arc<dyn QfcommitChain>);

impl NetHandler for NetQuorumBlockProcessor {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn extension(&self) -> Option<Extension> {
        Some(Extension::QuorumBlockProcessor)
    }

    fn process_message(
        &mut self,
        _cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        unreachable!("the registry calls process_message_result")
    }

    // Core: v24.0.0 src/llmq/blockprocessor.cpp:98-119 CQuorumBlockProcessor::ProcessMessage,
    // net_processing.cpp:5800-5807
    fn process_message_result(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<HandlerResult, Throw> {
        if *cmd != CommandString::QFCOMMIT {
            return Ok(HandlerResult::Done);
        }
        let mut data = payload;
        let qc: P2pCommitment = cx.read(&mut data)?;
        let id = cx.id();
        let hash = Hash256::from_lendian(commitment_hash(&qc));
        if cx.consume_getdata_response(&Inventory::new(InvType::QuorumFinalCommitment, hash))
            == GetDataResponse::Unrequested
        {
            tracing::debug!(target: "llmq", "CQuorumBlockProcessor::ProcessMessage -- unrequested commitment from peer={id}");
            cx.misbehaving(UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE, "unrequested quorum commitment");
            return Ok(HandlerResult::Done);
        }
        let facts = Facts {
            quorum: qc.quorum_hash(),
            llmq_type: qc.llmq_type().to_base(),
            valid: qc.valid_members().count_ones(),
            signers: qc.signers().count_ones(),
        };
        Ok(HandlerResult::Continue(Box::new(Pending {
            chain: Arc::clone(&self.0),
            send: Some((qc, cx.peer.common_version().get())),
            wait: None,
            answer: None,
            facts,
        })))
    }

    // Core: v24.0.0 src/net_processing.cpp:2438-2439
    fn already_have(&self, inv: &Inventory) -> bool {
        inv.inv_type() == InvType::QuorumFinalCommitment && self.0.has(&inv.inv_hash().to_lendian())
    }

    // Core: v24.0.0 src/net_processing.cpp:3029-3035
    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        if inv.inv_type() != InvType::QuorumFinalCommitment {
            return false;
        }
        let Some(qc) = self.0.get(&inv.inv_hash().to_lendian()) else {
            return false;
        };
        let mut payload = Vec::new();
        write_commitment(&qc, &mut payload);
        cx.push(OutMsg::new(CommandString::QFCOMMIT, payload));
        true
    }
}

/// What Core's log line after `Verify` reports.
struct Facts {
    quorum: QuorumHash,
    llmq_type: u8,
    valid: u64,
    signers: u64,
}

/// A solicited commitment's verdict: sent to V on the first poll, outside the region, and
/// applied in the reopened one. A sender that disconnects, or a shutdown, ends the continuation
/// where it stands: before the first poll the commitment never reaches V; after V answers, an
/// accepted commitment stays in the pool unrelayed and is served to the peers that ask for it.
/// Core checks, pools and relays in one pass over the message; this is a deviation from it.
struct Pending {
    chain: Arc<dyn QfcommitChain>,
    send: Option<(P2pCommitment, i32)>,
    wait: Option<AnswerWait>,
    answer: Option<Answer>,
    facts: Facts,
}

impl PostRegion for Pending {
    fn poll(
        &mut self,
        _logic: &mut PeerLogic,
        _peer: &PeerHandle,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Interrupted>> {
        if let Some((qc, version)) = self.send.take() {
            self.wait = Some(self.chain.process(qc, version));
        }
        let Some(wait) = self.wait.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        let answer = std::task::ready!(wait.as_mut().poll(cx));
        self.wait = None;
        self.answer = Some(answer);
        Poll::Ready(Ok(()))
    }

    // Core: v24.0.0 src/llmq/blockprocessor.cpp:121-211, its result applied by PostProcessMessage
    // (net_processing.cpp:3826-3842)
    fn tail(self: Box<Self>, cx: &mut HandlerCx<'_>) -> Result<(), Throw> {
        let id = cx.id();
        let Facts {
            quorum,
            llmq_type,
            valid,
            signers,
        } = self.facts;
        match self.answer.unwrap_or(Answer::NoAnswer) {
            Answer::Verdict(QfcommitOutcome::Misbehaving(score)) => cx.misbehaving(score, ""),
            Answer::Verdict(QfcommitOutcome::Ignored) | Answer::NoAnswer => {}
            Answer::Verdict(QfcommitOutcome::Accepted(relay)) => {
                tracing::debug!(target: "llmq", "CQuorumBlockProcessor::ProcessMessage -- received commitment for quorum {quorum}:{llmq_type}, validMembers={valid}, signers={signers}, peer={id}");
                if let Some(hash) = relay {
                    let inv =
                        Inventory::new(InvType::QuorumFinalCommitment, Hash256::from_lendian(hash));
                    cx.post_process(MessageProcessingResult::relay(inv));
                }
            }
            Answer::NoVerdict(why) => {
                tracing::debug!(target: "llmq", "CQuorumBlockProcessor::ProcessMessage -- no verdict for quorum {quorum}:{llmq_type} ({why}), peer={id}");
            }
            Answer::ReadFailed(error) => return Err(read_failed(&error)),
        }
        Ok(())
    }
}

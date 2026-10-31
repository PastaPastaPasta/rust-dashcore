//! A DKG session (`CDKGSession`, `ActiveDKGSession`; LQ4 §2.2, §3.2-§3.3): its inputs, the
//! environment it reaches through injected traits, and its state.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use dash_evo::llmq::{OperatorKeyView, QuorumMembers};
use dash_evo::types::ProTxHash;
use dash_params::LlmqParams;

use super::bls::{
    BlsCtx, BlsIesMultiBlobs, BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue, member_id,
    verify_contribution_share, verify_contribution_shares,
};
use super::rng::DkgRng;
use super::sim::SimErrorRates;
use super::wire::{Complaint, Contribution, Justification, LlmqType, QuorumHash};

/// The quorum's base block (`m_quorum_base_block_index`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuorumBase {
    /// Its hash, the quorum hash.
    pub hash: QuorumHash,
    /// Its height.
    pub height: i32,
}

/// What `CDKGSession::Init` reads (`l/dkgsession.cpp:90-155`), for one session = Core's
/// `CDKGSession`/`ActiveDKGSession` for (type, base, quorumIndex) on an active masternode.
/// Observers run no session (`l/net_dkg.cpp:511-518`, :739).
#[derive(Clone, Debug)]
pub struct SessionInit<'a, E> {
    /// The type's effective parameters (regtest overrides applied, LQ4 §4.1).
    pub params: &'a LlmqParams,
    /// The quorum's base block.
    pub base: QuorumBase,
    /// The handler's quorum index.
    pub quorum_index: i32,
    /// `GetAllQuorumMembers` at the base, DK1's access #1 (LQ4 §3.4.1), in Core's order.
    pub members: QuorumMembers<E>,
    /// The active masternode's proTxHash; `None` is Core's null (the masternode is not ready).
    pub active_pro_tx: Option<ProTxHash>,
    /// `IsQuorumRotationEnabled(params, base)`.
    pub rotation_enabled: bool,
    /// `DeploymentActiveAfter(base, V19)`: the session signs under the legacy scheme iff not
    /// (`a/dkgsession.cpp:34`).
    pub v19_after_base: bool,
}

/// Why `Init` refused a session; DK1 then leaves the handler without one (rc.4
/// `a/dkgsessionhandler.cpp:94-105`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitRefusal {
    /// "not enough members (%d < %d), aborting init" (`l/dkgsession.cpp:129-132`).
    NotEnoughMembers {
        /// The member count.
        have: usize,
        /// `minSize`.
        min: i32,
        /// The lines `Init` logged, its refusal last: Core writes them when `Init` returns.
        events: Vec<DkgEvent>,
    },
}

/// A call Core's handler cannot make; DK1 treats it as a fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DkgFault {
    /// A start function called twice or out of phase order.
    PhaseOrder,
}

/// LQ5's `GetQuorumRelayMembers(params, base, me, onlyOutbound = true)` over CH7b's facade,
/// DK1's access #2 (LQ4 §3.4.1). `init` calls it once, only for a member, after the minSize
/// check and `InitLocalSessionStatus` (`l/dkgsession.cpp:135-146`).
pub trait RelayTopology {
    /// The members `me` relays to.
    fn relay_members(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        me: &ProTxHash,
    ) -> BTreeSet<ProTxHash>;
}

/// The active masternode's operator key (AM1; `CActiveMasternodeManager`).
pub trait OperatorKey {
    /// `Sign(hash, is_legacy)` (`active/masternode.cpp:290-294`).
    fn sign(&self, hash: &[u8; 32], legacy: bool) -> BlsSigValue;
    /// `Decrypt(obj, idx, out)` (`active/masternode.cpp:279-284`): the blob at `idx` decrypted
    /// with the operator key and read as a secret key under `flag`, Core's process-wide scheme.
    fn decrypt_share(
        &self,
        blobs: &BlsIesMultiBlobs,
        idx: usize,
        flag: BlsSchemeKind,
    ) -> Option<BlsSkValue>;
}

/// dkgdb (DK1), row for row with `l/dkgsessionmgr.cpp:125-192`.
///
/// Infallible by contract: a backend error (Core's dbwrapper throws) fail-stops DK1's
/// implementation and is never reported here. An absent row is not an error; the reads that
/// meet one (`GetVerifiedContributions`, :163-169, :180-183) arrive with LQ4b-1.
pub trait DkgStore {
    /// `WriteVerifiedVvecContribution` (`qdkg_V`).
    fn write_vvec(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, vvec: &[BlsPkValue]);
    /// `WriteVerifiedSkContribution` (`qdkg_S`).
    fn write_sk(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, sk: &BlsSkValue);
    /// `WriteEncryptedContributions` (`qdkg_E`).
    fn write_encrypted(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        who: &ProTxHash,
        blobs: &BlsIesMultiBlobs,
    );
}

/// A bit of `CDKGDebugSessionStatus::statusBits` (`l/debug.h:64-73`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatusBit {
    /// `sentContributions`.
    SentContributions,
    /// `sentComplaint`.
    SentComplaint,
    /// `sentJustification`.
    SentJustification,
    /// `sentPrematureCommitment`.
    SentPrematureCommitment,
    /// `aborted`.
    Aborted,
}

/// A bit of `CDKGDebugMemberStatus::statusBits` (`l/debug.h:33-45`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberStatusBit {
    /// `bad`.
    Bad,
    /// `weComplain`.
    WeComplain,
    /// `receivedContribution`.
    ReceivedContribution,
    /// `receivedComplaint`.
    ReceivedComplaint,
    /// `receivedJustification`.
    ReceivedJustification,
    /// `receivedPrematureCommitment`.
    ReceivedPrematureCommitment,
}

/// `CDKGDebugManager`'s local status, which `quorumdkgstatus` shows.
pub trait DkgDebugSink {
    /// `InitLocalSessionStatus(params, quorumIndex, quorumHash, quorumHeight)`.
    fn init_local_session_status(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        base: &QuorumHash,
        base_height: i32,
    );
    /// `UpdateLocalSessionStatus`, setting `bit`.
    fn update_local_session_status(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        bit: SessionStatusBit,
    );
    /// `UpdateLocalMemberStatus` of member `idx`, setting `bit`.
    fn update_local_member_status(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        idx: usize,
        bit: MemberStatusBit,
    );
    /// `UpdateLocalMemberStatus` of member `idx`, adding member `from` to its
    /// `complaintsFromMembers` (`l/dkgsession.cpp:340-342`). Core's update returns
    /// `emplace(…).second`, so a `from` already present leaves the status as it is; the session
    /// calls this again for a complaint whose session hash it processes again (m6 (d)).
    fn add_local_member_complaint(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        idx: usize,
        from: usize,
    );
}

/// An order-preserving map over the independent BLS work of one call (LQ4 §3.6). DK1 runs it
/// on its BLS pool; [`SerialRunner`] keeps the pure crate pool-free (DEC-79).
pub trait DkgBlsRunner {
    /// `VerifyContributionShares(forId, vvecs, skShares)`: one verdict per contribution.
    fn verify_contribution_shares(
        &self,
        for_id: &[u8; 32],
        vvecs: &[&[BlsPkValue]],
        shares: &[&BlsSkValue],
        flag: BlsSchemeKind,
    ) -> Vec<bool>;

    /// `AsyncVerifyContributionShare(forId, vvec, sk)` for each `(forId, sk)`, all against one
    /// `vvec` (`l/dkgsession.cpp:453-460`): one verdict each, `false` for every share when the
    /// vector was never received.
    fn verify_justification_shares(
        &self,
        vvec: Option<&[BlsPkValue]>,
        shares: &[(&[u8; 32], &BlsSkValue)],
        flag: BlsSchemeKind,
    ) -> Vec<bool> {
        shares
            .iter()
            .map(|(id, sk)| vvec.is_some_and(|v| verify_contribution_share(id, v, sk, flag)))
            .collect()
    }
}

/// The serial [`DkgBlsRunner`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SerialRunner;

impl DkgBlsRunner for SerialRunner {
    fn verify_contribution_shares(
        &self,
        for_id: &[u8; 32],
        vvecs: &[&[BlsPkValue]],
        shares: &[&BlsSkValue],
        flag: BlsSchemeKind,
    ) -> Vec<bool> {
        verify_contribution_shares(for_id, vvecs, shares, flag)
    }
}

/// Everything a step may touch outside the session, all injected (LQ4 §3.2). `FinalVerifier`
/// arrives with LQ4b-1 (LQ4a-1 D-2).
pub struct DkgEnv<'a> {
    /// Core's process-wide scheme (`bls::bls_legacy_scheme`) for this step: the read, hash and
    /// verify scheme (m11).
    pub bls: BlsCtx,
    /// The session's randomness.
    pub rng: &'a mut dyn DkgRng,
    /// The operator key.
    pub key: &'a dyn OperatorKey,
    /// dkgdb.
    pub store: &'a mut dyn DkgStore,
    /// The debug status.
    pub debug: &'a mut dyn DkgDebugSink,
    /// The simulated error rates as of this call. Each call is one uninterrupted run of Core
    /// decisions on the phase thread, so a snapshot per call equals Core's read at each
    /// decision; DK1 rereads between calls.
    pub sim: &'a SimErrorRates,
    /// The BLS runner.
    pub runner: &'a dyn DkgBlsRunner,
}

/// One line the session logs, without the cxxtimer fields (`time=`, `totalTime=`), which have no
/// deterministic value.
///
/// Core writes most lines through a `CDKGLogger`, which batches a call's lines into one record
/// under the header `QuorumDKG(type=…, qIndex=…, h=…, member=…)` and writes it when the call
/// returns or flushes. The lines of one record share its `record` id and are contiguous, in the
/// order Core writes them; the header's fields are the session's. A line Core writes on its own
/// (`LogPrint`) has no record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DkgEvent {
    /// The `CDKGLogger` record the line is batched in, numbered from 0 per session; `None` for
    /// a line written on its own.
    pub record: Option<u32>,
    /// The logging function, e.g. `ReceiveMessage`.
    pub func: &'static str,
    /// The line.
    pub text: String,
}

impl fmt::Display for DkgEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -- {}", self.func, self.text)
    }
}

/// The start functions Core's handler has called, in phase order (LQ4 §2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Initialized,
    Contributed,
    Complained,
    Justified,
}

impl Stage {
    const fn next(self) -> Option<Self> {
        match self {
            Self::Initialized => Some(Self::Contributed),
            Self::Contributed => Some(Self::Complained),
            Self::Complained => Some(Self::Justified),
            Self::Justified => None,
        }
    }
}

/// The active masternode's place in the session.
#[derive(Clone, Copy, Debug)]
struct Me {
    idx: usize,
    pro_tx: ProTxHash,
}

/// A DKG session for one (type, base, quorumIndex).
#[derive(Clone, Debug)]
pub struct DkgSession<E> {
    params: LlmqParams,
    base: QuorumBase,
    quorum_index: i32,
    rotation_enabled: bool,
    v19_after_base: bool,
    members: QuorumMembers<E>,
    /// `membersMap`: the first index of each proTxHash.
    members_map: BTreeMap<ProTxHash, usize>,
    /// `memberIds`: `CBLSId(proTxHash)` per member.
    member_ids: Vec<[u8; 32]>,
    me: Option<Me>,
    relay_members: BTreeSet<ProTxHash>,
    /// Each member's `CDKGMember` state, with what the session received from it.
    member_state: Vec<MemberState>,
    /// The last start function called: Core's handler calls each once, in phase order.
    stage: Stage,
    /// `vvecContribution`: our polynomial's verification vector.
    own_vvec: Vec<BlsPkValue>,
    /// `m_sk_contributions`: each member's true share of our polynomial, which a lie never
    /// replaces.
    own_shares: Vec<BlsSkValue>,
    /// `contributions`: each contribution recorded, by session hash.
    contributions: BTreeMap<[u8; 32], Contribution>,
    /// `complaints`: each complaint recorded, by session hash.
    complaints: BTreeMap<[u8; 32], Complaint>,
    /// `justifications`: each justification recorded, by session hash.
    justifications: BTreeMap<[u8; 32], Justification>,
    /// `pendingContributionVerifications`: the members whose decrypted share waits for
    /// `VerifyPendingContributions`, in receive order.
    pending: Vec<usize>,
    events: Vec<DkgEvent>,
    /// The record `log` batches into.
    record: Option<u32>,
    /// The next record's id.
    records: u32,
}

/// A member's `CDKGMember` state (`l/dkgsession.h:39-60`): the session hashes of the messages
/// recorded from it, by kind, and its flags; with the session's per-member slots of
/// `receivedVvecs`, `receivedSkContributions` and `vecEncryptedContributions`.
#[derive(Clone, Debug, Default)]
pub(crate) struct MemberState {
    pub(crate) contributions: BTreeSet<[u8; 32]>,
    pub(crate) complaints: BTreeSet<[u8; 32]>,
    pub(crate) justifications: BTreeSet<[u8; 32]>,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "batched with its Receive in LQ4a-2 or LQ4b-1")
    )]
    pub(crate) premature_commitments: BTreeSet<[u8; 32]>,
    pub(crate) bad: bool,
    pub(crate) bad_connection: bool,
    pub(crate) we_complain: bool,
    pub(crate) someone_complain: bool,
    /// `badMemberVotes`: the members whose complaint marked this one bad.
    pub(crate) bad_member_votes: BTreeSet<ProTxHash>,
    /// `complaintsFromOthers`: the members complaining about this one's contribution whom no
    /// justification has answered yet.
    pub(crate) complaints_from_others: BTreeSet<ProTxHash>,
    pub(crate) received_vvec: Option<Vec<BlsPkValue>>,
    pub(crate) received_sk: Option<BlsSkValue>,
    pub(crate) encrypted: Option<BlsIesMultiBlobs>,
}

impl<E: OperatorKeyView> DkgSession<E> {
    /// `CDKGSession::Init` (`l/dkgsession.cpp:90-155`): the members and their ids, our place
    /// among them, then, for a session with at least `minSize` members, `InitLocalSessionStatus`
    /// and the relay members, both for a member only.
    ///
    /// With `BCLog::LLMQ` on, Core writes two more lines into this record, which DK1 inserts as
    /// it holds their inputs (the cycle base height among them): for a rotated type,
    /// `DKGComposition h[…] i[…] DKG:[…]` as the record's first line (`l/dkgsession.cpp:120-128`),
    /// and for a member `forMember[…] relayMembers[…]` (from [`Self::relay_members`]) right before
    /// "initialized as member" (:137-145).
    ///
    /// # Errors
    ///
    /// [`InitRefusal::NotEnoughMembers`] below `minSize`.
    pub fn init(
        init: SessionInit<'_, E>,
        topo: &mut dyn RelayTopology,
        debug: &mut dyn DkgDebugSink,
    ) -> Result<Self, InitRefusal> {
        let members = init.members;
        let mut members_map = BTreeMap::new();
        for (i, m) in members.iter().enumerate() {
            members_map.entry(*m.pro_tx_hash()).or_insert(i);
        }
        let member_ids = members.iter().map(|m| member_id(m.pro_tx_hash())).collect();
        let me = init.active_pro_tx.filter(|pro_tx| !pro_tx.is_null()).and_then(|pro_tx| {
            members.iter().position(|m| *m.pro_tx_hash() == pro_tx).map(|idx| Me {
                idx,
                pro_tx,
            })
        });
        let members_len = members.len();
        let mut session = Self {
            params: *init.params,
            base: init.base,
            quorum_index: init.quorum_index,
            rotation_enabled: init.rotation_enabled,
            v19_after_base: init.v19_after_base,
            members,
            members_map,
            member_ids,
            me,
            relay_members: BTreeSet::new(),
            member_state: vec![MemberState::default(); members_len],
            stage: Stage::Initialized,
            own_vvec: Vec::new(),
            own_shares: Vec::new(),
            contributions: BTreeMap::new(),
            complaints: BTreeMap::new(),
            justifications: BTreeMap::new(),
            pending: Vec::new(),
            events: Vec::new(),
            record: None,
            records: 0,
        };
        session.begin_record();
        let have = session.members.len();
        let min = session.params.min_size;
        if have < usize::try_from(min).unwrap_or(0) {
            session.log("Init", format!("not enough members ({have} < {min}), aborting init"));
            return Err(InitRefusal::NotEnoughMembers {
                have,
                min,
                events: session.take_events(),
            });
        }
        if let Some(me) = session.me {
            let t = session.params.llmq_type;
            debug.init_local_session_status(
                t,
                session.quorum_index,
                &session.base.hash,
                session.base.height,
            );
            session.relay_members = topo.relay_members(t, &session.base.hash, &me.pro_tx);
            session.log("Init", format!("initialized as member. mns={have}"));
        } else {
            session.log("Init", format!("initialized as observer. mns={have}"));
        }
        Ok(session)
    }

    /// The type's parameters.
    pub fn params(&self) -> &LlmqParams {
        &self.params
    }

    /// The base block.
    pub fn base(&self) -> &QuorumBase {
        &self.base
    }

    /// The handler's quorum index.
    pub fn quorum_index(&self) -> i32 {
        self.quorum_index
    }

    /// `IsQuorumRotationEnabled(params, base)`, as given.
    pub fn rotation_enabled(&self) -> bool {
        self.rotation_enabled
    }

    /// `m_use_legacy_bls`: the session's signatures use the legacy scheme
    /// (`a/dkgsession.cpp:34`).
    pub fn signs_legacy(&self) -> bool {
        !self.v19_after_base
    }

    /// The members, in Core's order.
    pub fn members(&self) -> &[E] {
        &self.members
    }

    /// `GetMember(proTxHash)`'s index.
    pub fn member_index(&self, pro_tx: &ProTxHash) -> Option<usize> {
        self.members_map.get(pro_tx).copied()
    }

    /// `memberIds`.
    pub fn member_ids(&self) -> &[[u8; 32]] {
        &self.member_ids
    }

    /// `GetMyMemberIndex()`: `None` for an observer.
    pub fn my_index(&self) -> Option<usize> {
        self.me.map(|me| me.idx)
    }

    /// `AreWeMember()`.
    pub fn are_we_member(&self) -> bool {
        self.me.is_some()
    }

    /// `RelayMembers()`: empty for a non-member.
    pub fn relay_members(&self) -> &BTreeSet<ProTxHash> {
        &self.relay_members
    }

    /// Whether member `idx` is marked bad (`CDKGMember::bad`).
    #[cfg(any(test, feature = "reference"))]
    pub fn is_bad(&self, idx: usize) -> bool {
        self.member_state[idx].bad
    }

    /// Whether we complain about member `idx` (`CDKGMember::weComplain`).
    #[cfg(any(test, feature = "reference"))]
    pub fn we_complain(&self, idx: usize) -> bool {
        self.member_state[idx].we_complain
    }

    /// Whether member `idx` is a bad connection (`CDKGMember::badConnection`).
    #[cfg(any(test, feature = "reference"))]
    pub fn is_bad_connection(&self, idx: usize) -> bool {
        self.member_state[idx].bad_connection
    }

    /// The members still complaining about member `idx`'s contribution
    /// (`CDKGMember::complaintsFromOthers`).
    #[cfg(any(test, feature = "reference"))]
    pub fn complaints_from_others(&self, idx: usize) -> &BTreeSet<ProTxHash> {
        &self.member_state[idx].complaints_from_others
    }

    /// The members whose complaint marked member `idx` bad (`CDKGMember::badMemberVotes`).
    #[cfg(any(test, feature = "reference"))]
    pub fn bad_member_votes(&self, idx: usize) -> &BTreeSet<ProTxHash> {
        &self.member_state[idx].bad_member_votes
    }

    /// Each member's `!bad`: `SendCommitment`'s `validMembers` once `VerifyAndCommit` has
    /// marked the members with open complaints (`a/dkgsession.cpp:482-487`).
    #[cfg(any(test, feature = "reference"))]
    pub fn valid_members(&self) -> Vec<bool> {
        self.member_state.iter().map(|m| !m.bad).collect()
    }

    /// Takes the session to the Contribute phase's end with `vvec` and the true `shares` as its
    /// own polynomial, without drawing: a transcript replay adopts the contribution the
    /// capture holds (LQ4 §4.3 T3-C), whose message then arrives through the batch as
    /// [`Sender::Own`].
    ///
    /// # Errors
    ///
    /// [`DkgFault::PhaseOrder`] after `contribute` or another adoption.
    #[cfg(any(test, feature = "reference"))]
    pub fn adopt_contribution(
        &mut self,
        vvec: Vec<BlsPkValue>,
        shares: Vec<BlsSkValue>,
    ) -> Result<(), DkgFault> {
        self.enter(Stage::Contributed)?;
        self.own_vvec = vvec;
        self.own_shares = shares;
        Ok(())
    }

    /// Moves to `next`, the stage right after the current one.
    fn enter(&mut self, next: Stage) -> Result<(), DkgFault> {
        if self.stage.next() != Some(next) {
            return Err(DkgFault::PhaseOrder);
        }
        self.stage = next;
        Ok(())
    }

    /// The members whose shares wait for `VerifyPendingContributions`, in push order.
    #[cfg(any(test, feature = "reference"))]
    pub fn pending(&self) -> &[usize] {
        &self.pending
    }

    /// Our polynomial's true shares (`m_sk_contributions`), empty before `contribute`.
    #[cfg(any(test, feature = "reference"))]
    pub fn own_shares(&self) -> &[BlsSkValue] {
        &self.own_shares
    }

    /// The lines logged since the last call.
    pub fn take_events(&mut self) -> Vec<DkgEvent> {
        std::mem::take(&mut self.events)
    }

    /// A new `CDKGLogger`: the lines `log` writes from now go in a new record. The record it
    /// replaces, for a nested logger to restore.
    fn begin_record(&mut self) -> Option<u32> {
        let id = self.records;
        self.records = self.records.wrapping_add(1);
        self.record.replace(id)
    }

    /// A line batched in the current record.
    fn log(&mut self, func: &'static str, text: impl Into<String>) {
        self.push_event(self.record, func, text.into());
    }

    /// A line Core writes on its own, outside any record.
    fn log_line(&mut self, func: &'static str, text: impl Into<String>) {
        self.push_event(None, func, text.into());
    }

    fn push_event(&mut self, record: Option<u32>, func: &'static str, text: String) {
        self.events.push(DkgEvent {
            record,
            func,
            text,
        });
    }
}

mod batch;
mod complain;
mod contrib;
mod justify;
mod preverify;

pub use batch::{BatchOutcome, DkgInv, DkgMsgKind, Misbehaviour, Relayed, Scored, Sender};
pub use preverify::DkgReject;

#[cfg(test)]
mod fixture;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod vectors;

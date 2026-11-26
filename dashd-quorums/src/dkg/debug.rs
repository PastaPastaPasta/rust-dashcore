//! `CDKGDebugManager` (DK1 plan §4 L2; DEC-1865 Q3): the local DKG status `quorum dkgstatus`
//! shows, which the handlers and their sessions update.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

use dash_evo::types::ProTxHash;
use dash_llmq::dkg::session::{DkgDebugSink, MemberStatusBit, SessionStatusBit};
use dash_params::{LlmqParams, LlmqType};
use dash_primitives::QuorumHash;
use dash_types::Numeric;
use serde_json::{Map, Value, json};

use super::seams::PhaseMarks;
use super::types::QuorumPhase;

/// `CDKGDebugMemberStatus`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemberStatus {
    /// `bad`.
    pub bad: bool,
    /// `weComplain`.
    pub we_complain: bool,
    /// `receivedContribution`.
    pub received_contribution: bool,
    /// `receivedComplaint`.
    pub received_complaint: bool,
    /// `receivedJustification`.
    pub received_justification: bool,
    /// `receivedPrematureCommitment`.
    pub received_premature_commitment: bool,
    /// `complaintsFromMembers`.
    pub complaints_from_members: BTreeSet<usize>,
}

/// `CDKGDebugSessionStatus`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStatus {
    /// `llmqType`.
    pub llmq_type: LlmqType,
    /// `quorumHash`.
    pub quorum_hash: QuorumHash,
    /// `quorumHeight`.
    pub quorum_height: u32,
    /// `phase`: 0 until the first `MarkPhaseAdvanced`.
    pub phase: i32,
    /// `sentContributions`.
    pub sent_contributions: bool,
    /// `sentComplaint`.
    pub sent_complaint: bool,
    /// `sentJustification`.
    pub sent_justification: bool,
    /// `sentPrematureCommitment`.
    pub sent_premature_commitment: bool,
    /// `aborted`.
    pub aborted: bool,
    /// `members`: the type's `size` entries.
    pub members: Vec<MemberStatus>,
}

/// What `ToJson` reads outside the status.
pub trait DebugJsonSource {
    /// `Params().GetLLMQ(t).has_value()`.
    fn has_llmq(&self, t: LlmqType) -> bool;
    /// `GetAllQuorumMembers(t, LookupBlockIndex(quorum_hash))`, read at detail level 2; `None`
    /// when the block is unknown.
    fn members(&self, t: LlmqType, quorum_hash: &QuorumHash) -> Option<Vec<ProTxHash>>;
    /// `FormatISO8601DateTime(time)`.
    fn format_time(&self, time: i64) -> String;
}

#[derive(Default)]
struct Status {
    time: i64,
    sessions: BTreeMap<(u8, i32), SessionStatus>,
}

type AdjustedTime = Box<dyn Fn() -> i64 + Send + Sync>;

/// `CDKGDebugManager`. Its lock is a leaf: nothing else is called under it.
pub struct DebugManager {
    sizes: BTreeMap<u8, u16>,
    adjusted_time: AdjustedTime,
    status: Mutex<Status>,
}

fn key(t: LlmqType, quorum_index: i32) -> (u8, i32) {
    (t.to_base(), quorum_index)
}

impl DebugManager {
    /// A manager with no sessions for the types `llmqs`, whose `size` gives a session's member
    /// count; `adjusted_time` is `GetAdjustedTime()`, in seconds.
    pub fn new(
        llmqs: impl IntoIterator<Item = (LlmqType, LlmqParams)>,
        adjusted_time: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        let mut sizes = BTreeMap::new();
        for (t, params) in llmqs {
            sizes.entry(t.to_base()).or_insert(params.size);
        }
        Self {
            sizes,
            adjusted_time: Box::new(adjusted_time),
            status: Mutex::new(Status::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// `GetSessionCount()`.
    pub fn session_count(&self) -> usize {
        self.lock().sessions.len()
    }

    /// The status of session (`t`, `quorum_index`), if any.
    pub fn session(&self, t: LlmqType, quorum_index: i32) -> Option<SessionStatus> {
        self.lock().sessions.get(&key(t, quorum_index)).cloned()
    }

    /// `localStatus.time`.
    pub fn time(&self) -> i64 {
        self.lock().time
    }

    /// Whether the manager was built with type `t`, whose `size` its sessions take.
    pub fn knows(&self, t: LlmqType) -> bool {
        self.sizes.contains_key(&t.to_base())
    }

    /// `ResetLocalSessionStatus`: drops the session's status.
    // Core: v24.0.0 src/llmq/debug.cpp:154-165 CDKGDebugManager::ResetLocalSessionStatus
    pub fn reset(&self, t: LlmqType, quorum_index: i32) {
        let now = (self.adjusted_time)();
        let mut status = self.lock();
        if status.sessions.remove(&key(t, quorum_index)).is_some() {
            status.time = now;
        }
    }

    /// `InitLocalSessionStatus`: the session's status anew, with `size` member entries.
    // Core: v24.0.0 src/llmq/debug.cpp:167-184 CDKGDebugManager::InitLocalSessionStatus
    pub fn init(&self, t: LlmqType, quorum_index: i32, quorum_hash: &QuorumHash, height: i32) {
        let size = self.sizes.get(&t.to_base()).copied().unwrap_or(0);
        let session = SessionStatus {
            llmq_type: t,
            quorum_hash: *quorum_hash,
            quorum_height: height as u32,
            phase: 0,
            sent_contributions: false,
            sent_complaint: false,
            sent_justification: false,
            sent_premature_commitment: false,
            aborted: false,
            members: vec![MemberStatus::default(); usize::from(size)],
        };
        self.lock().sessions.insert(key(t, quorum_index), session);
    }

    /// `UpdateLocalSessionStatus`: `f` on the session's status, if any; the time moves when it
    /// returns true.
    // Core: v24.0.0 src/llmq/debug.cpp:186-198 CDKGDebugManager::UpdateLocalSessionStatus
    pub fn update_session(
        &self,
        t: LlmqType,
        quorum_index: i32,
        f: impl FnOnce(&mut SessionStatus) -> bool,
    ) {
        let now = (self.adjusted_time)();
        let mut status = self.lock();
        let Some(session) = status.sessions.get_mut(&key(t, quorum_index)) else {
            return;
        };
        if f(session) {
            status.time = now;
        }
    }

    /// `UpdateLocalMemberStatus`: `f` on member `idx`'s status, if the session has one; the time
    /// moves when it returns true.
    ///
    /// # Panics
    ///
    /// When the session exists and `idx` is not below its type's `size`: Core's
    /// `members.at(idx)` throws, and the phase thread does not catch it.
    // Core: v24.0.0 src/llmq/debug.cpp:200-212 CDKGDebugManager::UpdateLocalMemberStatus
    pub fn update_member(
        &self,
        t: LlmqType,
        quorum_index: i32,
        idx: usize,
        f: impl FnOnce(&mut MemberStatus) -> bool,
    ) {
        let now = (self.adjusted_time)();
        let mut status = self.lock();
        let Some(session) = status.sessions.get_mut(&key(t, quorum_index)) else {
            return;
        };
        let size = session.members.len();
        let Some(member) = session.members.get_mut(idx) else {
            drop(status);
            panic!("UpdateLocalMemberStatus: member index {idx} out of range ({size} members)");
        };
        if f(member) {
            status.time = now;
        }
    }

    /// `MarkPhaseAdvanced`: the session's phase, the time moving only on a change.
    // Core: v24.0.0 src/llmq/debug.cpp:214-221 CDKGDebugManager::MarkPhaseAdvanced
    pub fn mark_phase_advanced(&self, t: LlmqType, quorum_index: i32, phase: QuorumPhase) {
        self.update_session(t, quorum_index, |s| {
            let changed = s.phase != phase as i32;
            s.phase = phase as i32;
            changed
        });
    }

    /// `MarkAborted`.
    // Core: v24.0.0 src/llmq/debug.cpp:223-229 CDKGDebugManager::MarkAborted
    pub fn mark_aborted(&self, t: LlmqType, quorum_index: i32) {
        self.update_session(t, quorum_index, |s| {
            s.aborted = true;
            true
        });
    }

    /// `ToJson(detailLevel)`, from a copy of the status taken under the lock, so that `src` is
    /// read with no lock held.
    // Core: v24.0.0 src/llmq/debug.cpp:126-152 CDKGDebugManager::ToJson
    pub fn to_json(&self, detail: i32, src: &dyn DebugJsonSource) -> Value {
        let (time, sessions) = {
            let status = self.lock();
            (status.time, status.sessions.clone())
        };
        let mut sessions_json = Vec::new();
        for ((_, quorum_index), session) in &sessions {
            let t = session.llmq_type;
            if !src.has_llmq(t) {
                continue;
            }
            sessions_json.push(json!({
                "llmqType": t.to_string(),
                "quorumIndex": quorum_index,
                "status": session_json(session, detail, src),
            }));
        }
        json!({ "time": time, "timeStr": src.format_time(time), "session": sessions_json })
    }
}

// Core: v24.0.0 src/llmq/debug.cpp:19-109 CDKGDebugSessionStatus::ToJson
fn session_json(s: &SessionStatus, detail: i32, src: &dyn DebugJsonSource) -> Value {
    let mut ret = Map::new();
    if !src.has_llmq(s.llmq_type) || s.quorum_hash == QuorumHash::default() {
        return Value::Object(ret);
    }
    let dmn_members = if detail == 2 {
        src.members(s.llmq_type, &s.quorum_hash).unwrap_or_default()
    } else {
        Vec::new()
    };
    ret.insert("llmqType".into(), json!(s.llmq_type.to_base()));
    ret.insert("quorumHash".into(), json!(s.quorum_hash.to_string()));
    ret.insert("quorumHeight".into(), json!(s.quorum_height));
    ret.insert("phase".into(), json!(s.phase));
    ret.insert("sentContributions".into(), json!(s.sent_contributions));
    ret.insert("sentComplaint".into(), json!(s.sent_complaint));
    ret.insert("sentJustification".into(), json!(s.sent_justification));
    ret.insert("sentPrematureCommitment".into(), json!(s.sent_premature_commitment));
    ret.insert("aborted".into(), json!(s.aborted));

    type Flag = fn(&MemberStatus) -> bool;
    let lists: [(&str, Flag); 6] = [
        ("badMembers", |m| m.bad),
        ("weComplain", |m| m.we_complain),
        ("receivedContributions", |m| m.received_contribution),
        ("receivedComplaints", |m| m.received_complaint),
        ("receivedJustifications", |m| m.received_justification),
        ("receivedPrematureCommitments", |m| m.received_premature_commitment),
    ];
    for (name, flag) in lists {
        let set = s.members.iter().enumerate().filter(|(_, m)| flag(m)).map(|(i, _)| i);
        let value = match detail {
            0 => json!(set.count()),
            1 => Value::Array(set.map(|i| json!(i)).collect()),
            2 => Value::Array(
                set.map(|i| {
                    let mut a = Map::new();
                    a.insert("memberIndex".into(), json!(i));
                    if let Some(dmn) = dmn_members.get(i) {
                        a.insert("proTxHash".into(), json!(dmn.to_string()));
                    }
                    Value::Object(a)
                })
                .collect(),
            ),
            _ => Value::Array(Vec::new()),
        };
        ret.insert(name.into(), value);
    }
    if detail == 2 {
        let all = dmn_members.iter().map(|m| json!(m.to_string())).collect();
        ret.insert("allMembers".into(), Value::Array(all));
    }
    Value::Object(ret)
}

impl PhaseMarks for DebugManager {
    fn reset_local_session_status(&self, t: LlmqType, quorum_index: i32) {
        self.reset(t, quorum_index);
    }

    fn mark_phase_advanced(&self, t: LlmqType, quorum_index: i32, phase: QuorumPhase) {
        DebugManager::mark_phase_advanced(self, t, quorum_index, phase);
    }
}

impl DkgDebugSink for &DebugManager {
    // Core: v24.0.0 src/llmq/dkgsession.cpp:137 CDKGSession::Init
    fn init_local_session_status(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        base: &QuorumHash,
        base_height: i32,
    ) {
        self.init(t, quorum_index, base, base_height);
    }

    // Core: v24.0.0 src/active/dkgsession.cpp:132,355,445,626 ActiveDKGSession::Send*
    fn update_local_session_status(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        bit: SessionStatusBit,
    ) {
        self.update_session(t, quorum_index, |s| {
            match bit {
                SessionStatusBit::SentContributions => s.sent_contributions = true,
                SessionStatusBit::SentComplaint => s.sent_complaint = true,
                SessionStatusBit::SentJustification => s.sent_justification = true,
                SessionStatusBit::SentPrematureCommitment => s.sent_premature_commitment = true,
                SessionStatusBit::Aborted => s.aborted = true,
            }
            true
        });
    }

    // Core: v24.0.0 src/llmq/dkgsession.cpp:257,628,744,776 CDKGSession; src/active/dkgsession.cpp:200
    fn update_local_member_status(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        idx: usize,
        bit: MemberStatusBit,
    ) {
        self.update_member(t, quorum_index, idx, |m| {
            match bit {
                MemberStatusBit::Bad => m.bad = true,
                MemberStatusBit::WeComplain => m.we_complain = true,
                MemberStatusBit::ReceivedContribution => m.received_contribution = true,
                MemberStatusBit::ReceivedComplaint => m.received_complaint = true,
                MemberStatusBit::ReceivedJustification => m.received_justification = true,
                MemberStatusBit::ReceivedPrematureCommitment => {
                    m.received_premature_commitment = true;
                }
            }
            true
        });
    }

    // Core: v24.0.0 src/llmq/dkgsession.cpp:349-351 CDKGSession::ReceiveMessage
    fn add_local_member_complaint(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        idx: usize,
        from: usize,
    ) {
        self.update_member(t, quorum_index, idx, |m| m.complaints_from_members.insert(from));
    }
}

#[cfg(test)]
mod tests;

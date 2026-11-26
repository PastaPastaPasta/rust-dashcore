//! Core's quorum data recovery planner, start-offset calculation and per-quorum state machine.
//! Time is passed in as `now` seconds, and sleeps are returned in [`Next`] rather than taken.

use std::time::Duration;

use dash_consensus::primitives::BlockHash;
use dash_evo::types::ProTxHash;
use dash_params::LlmqType;
use dash_types::Numeric;

use super::source::{QuorumData, ValidMnsAt};
use super::tracker::{Key, Request, Status, Tracker};
use super::{Act, PeerId, QuorumRef, Role, SyncMode, TipRef};
use crate::dkg::wire::QuorumDataRequest;

/// Seconds before a recovery job advances to the next member, and the unsynced retry interval.
// Core: v24.0.0 src/llmq/net_quorum.cpp:545 nRequestTimeout
pub const REQUEST_TIMEOUT: i64 = 10;

const SYNC_SLEEP: Duration = Duration::from_secs(10);
const LOOP_SLEEP: Duration = Duration::from_secs(1);

/// Per-quorum decision of [`plan_tip`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Recover a missing verification vector and/or secret key share as a valid quorum member.
    SkShare {
        q: QuorumRef,
        mask: u16,
        trigger: TipRef,
    },
    /// Sync a missing verification vector under `-llmq-qvvec-sync`.
    VvecSync {
        q: QuorumRef,
    },
    /// Nothing to request for this quorum at this tip; `caller` names the Core function that logs
    /// it (`"TriggerQuorumDataRecoveryThreads"` for a valid member, `"TryStartVvecSyncThread"`
    /// otherwise).
    NoDataNeeded {
        q: QuorumRef,
        caller: &'static str,
    },
}

/// Outcome of claiming a quorum's recovery slot before enqueueing a [`Plan`].
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// No job is enqueued (`NoDataNeeded` or an empty `validMembers` bitset).
    Skip,
    /// Another recovery job already holds the quorum's claim; carries Core's log line.
    AlreadyRunning(&'static str),
    /// The recovery claim is held; ready to enqueue onto the worker pool.
    Claimed(ClaimedJob),
}

/// A [`Plan`] that holds its quorum's recovery claim and awaits worker execution.
#[derive(Debug, PartialEq, Eq)]
pub enum ClaimedJob {
    SkShare {
        q: QuorumRef,
        mask: u16,
        trigger: TipRef,
    },
    VvecSync {
        q: QuorumRef,
    },
}

impl Plan {
    /// Core's `"No data needed"` log line at `height` when this plan is [`Self::NoDataNeeded`].
    // Core: v24.0.0 src/llmq/net_quorum.cpp:526-528,667-668 NetQuorum::TriggerQuorumDataRecoveryThreads / TryStartVvecSyncThread
    pub fn no_data_needed_log(&self, height: i32) -> Option<String> {
        match *self {
            Self::NoDataNeeded {
                q,
                caller,
            } => Some(format!(
                "NetQuorum::{caller} -- No data needed from ({}, {}) at height {height}",
                q.llmq_type.to_base(),
                q.quorum_hash,
            )),
            Self::SkShare {
                ..
            }
            | Self::VvecSync {
                ..
            } => None,
        }
    }

    /// Claims exclusive recovery on `quorum` before enqueueing onto the worker pool.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:640-647,672-680 NetQuorum::StartVvecSyncThread / StartSkShareRecoveryThread
    pub fn claim<Q: QuorumData>(self, quorum: &Q) -> Claim {
        match self {
            Self::NoDataNeeded {
                ..
            } => Claim::Skip,
            Self::VvecSync {
                q,
            } => {
                if quorum.size() == 0 {
                    return Claim::Skip;
                }
                if !quorum.try_claim_recovery() {
                    return Claim::AlreadyRunning(
                        "NetQuorum::StartVvecSyncThread -- Already running",
                    );
                }
                Claim::Claimed(ClaimedJob::VvecSync {
                    q,
                })
            }
            Self::SkShare {
                q,
                mask,
                trigger,
            } => {
                if quorum.size() == 0 {
                    return Claim::Skip;
                }
                if !quorum.try_claim_recovery() {
                    return Claim::AlreadyRunning(
                        "NetQuorum::StartSkShareRecoveryThread -- Already running",
                    );
                }
                Claim::Claimed(ClaimedJob::SkShare {
                    q,
                    mask,
                    trigger,
                })
            }
        }
    }
}

impl ClaimedJob {
    /// Builds the [`RecoveryJob`] when a worker begins running it, computing [`start_offset`] at
    /// the trigger block for `SkShare`.
    ///
    /// Core's `GetQuorumRecoveryStartOffset` (`net_quorum.cpp:688-709`) always computes an offset
    /// from `GetListForBlock`; when `valid_mns` has no list for the trigger block (an unavailable
    /// read), this releases the claim and returns `None` (no job, per PLAN-QM1b §0 R-g).
    // Core: v24.0.0 src/llmq/net_quorum.cpp:648-651,682-685,688-709 NetQuorum::StartVvecSyncThread / StartSkShareRecoveryThread / GetQuorumRecoveryStartOffset
    pub fn start<Q: QuorumData, V: ValidMnsAt>(
        self,
        quorum: &Q,
        valid_mns: &V,
        me: ProTxHash,
    ) -> Option<RecoveryJob> {
        match self {
            Self::VvecSync {
                q,
            } => Some(RecoveryJob::new(
                q,
                quorum,
                Request::QUORUM_VERIFICATION_VECTOR,
                ProTxHash::default(),
                0,
            )),
            Self::SkShare {
                q,
                mask,
                trigger,
            } => {
                let Some(enabled) = valid_mns.valid_pro_tx_hashes(&trigger.hash) else {
                    quorum.release_recovery();
                    return None;
                };
                let offset = start_offset(&enabled, &me, quorum.size());
                Some(RecoveryJob::new(q, quorum, mask, me, offset))
            }
        }
    }

    /// Releases `quorum`'s recovery claim when a queued job is dropped before starting.
    pub fn release<Q: QuorumData>(self, quorum: &Q) {
        quorum.release_recovery();
    }
}

/// Core's per-tip `"Process block"` log line when recovery is enabled and the node has a quorum
/// role (`None` otherwise).
// Core: v24.0.0 src/llmq/net_quorum.cpp:392,501-506 NetQuorum::TriggerQuorumDataRecoveryThreads
pub fn process_block_log(role: Role, enabled: bool, block: BlockHash) -> Option<String> {
    if !enabled {
        return None;
    }
    let pro_tx = match role {
        Role::Masternode {
            pro_tx,
            ..
        } => pro_tx.unwrap_or_default(),
        Role::Observer => ProTxHash::default(),
        Role::None => return None,
    };
    Some(format!(
        "NetQuorum::TriggerQuorumDataRecoveryThreads -- Process block {block} as protx_hash={pro_tx}"
    ))
}

/// Returns the per-quorum recovery decisions for `scans` at `trigger`. Pair with
/// [`process_block_log`] and [`Plan::no_data_needed_log`] for Core's tip log lines.
// Core: v24.0.0 src/llmq/net_quorum.cpp:499-535,654-670 NetQuorum::TriggerQuorumDataRecoveryThreads / TryStartVvecSyncThread
pub fn plan_tip<Q: QuorumData>(
    role: Role,
    enabled: bool,
    sync: &[(LlmqType, SyncMode)],
    scans: &[(LlmqType, Vec<(BlockHash, Q)>)],
    trigger: TipRef,
) -> Vec<Plan> {
    if !enabled {
        return Vec::new();
    }
    let (is_masternode, local_pro_tx) = match role {
        Role::Masternode {
            pro_tx,
            ..
        } => (true, pro_tx),
        Role::Observer => (false, None),
        Role::None => return Vec::new(),
    };

    let mut out = Vec::new();
    for (llmq_type, quorums) in scans {
        let type_member = is_masternode
            && local_pro_tx.is_some_and(|me| quorums.iter().any(|(_, q)| q.is_valid_member(&me)));
        let sync_mode = sync.iter().find(|(t, _)| t == llmq_type).map(|(_, mode)| *mode);

        for (quorum_hash, quorum) in quorums {
            let q = QuorumRef {
                llmq_type: *llmq_type,
                quorum_hash: *quorum_hash,
            };
            if is_masternode && local_pro_tx.is_some_and(|me| quorum.is_valid_member(&me)) {
                let mut mask = 0;
                if quorum.vvec().is_none() {
                    mask |= Request::QUORUM_VERIFICATION_VECTOR;
                }
                if !quorum.has_sk_share() {
                    mask |= Request::ENCRYPTED_CONTRIBUTIONS;
                }
                if mask != 0 {
                    out.push(Plan::SkShare {
                        q,
                        mask,
                        trigger,
                    });
                } else {
                    out.push(Plan::NoDataNeeded {
                        q,
                        caller: "TriggerQuorumDataRecoveryThreads",
                    });
                }
            } else {
                if quorum.recovery_running() {
                    continue;
                }
                let sync_current = match sync_mode {
                    Some(SyncMode::Always) => true,
                    Some(SyncMode::OnlyIfTypeMember) => type_member,
                    None => false,
                };
                if sync_current && quorum.vvec().is_none() {
                    out.push(Plan::VvecSync {
                        q,
                    });
                } else {
                    out.push(Plan::NoDataNeeded {
                        q,
                        caller: "TryStartVvecSyncThread",
                    });
                }
            }
        }
    }
    out
}

/// Returns `me`'s index in `enabled_at_trigger` sorted by raw-byte memcmp (0 if absent), modulo
/// `quorum_size` (`qc->validMembers.size()`).
// Core: v24.0.0 src/llmq/net_quorum.cpp:688-709 NetQuorum::GetQuorumRecoveryStartOffset; src/uint256.h:55-59 base_blob::Compare
pub fn start_offset(enabled_at_trigger: &[ProTxHash], me: &ProTxHash, quorum_size: usize) -> usize {
    if quorum_size == 0 {
        return 0;
    }
    let mut sorted = enabled_at_trigger.to_vec();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let index = sorted.iter().position(|h| h == me).unwrap_or(0);
    index % quorum_size
}

/// What the runtime does after applying a [`Poll`]'s actions and logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Sleep for the given duration (waking early on interrupt), then poll again.
    Sleep(Duration),
    /// The job has finished ([`RecoveryJob::poll_quorum`] releases the quorum's recovery claim once
    /// on entering `Done`, before the `"Done"` log line, matching `net_quorum.cpp:636-637`).
    Done,
}

/// One step of [`RecoveryJob::poll`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Poll {
    pub acts: Vec<Act>,
    pub log: Vec<String>,
    pub next: Next,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Init,
    SyncWait,
    RequestLoop,
    Connecting,
    Finished,
}

/// State machine of one `DataRecoveryThread` invocation.
#[derive(Debug)]
pub struct RecoveryJob {
    quorum: QuorumRef,
    targets: Vec<ProTxHash>,
    init_mask: u16,
    cur_mask: u16,
    pro_tx: ProTxHash,
    start_offset: usize,
    tries: usize,
    last_success: i64,
    current_member: Option<ProTxHash>,
    phase: Phase,
}

impl RecoveryJob {
    /// Creates a recovery job for `quorum`, targeting its valid members other than `pro_tx` in
    /// raw-byte memcmp order.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:537-570 NetQuorum::DataRecoveryThread; src/uint256.h:55-59 base_blob::Compare
    pub fn new<Q: QuorumData>(
        quorum: QuorumRef,
        data: &Q,
        data_mask: u16,
        pro_tx: ProTxHash,
        start_offset: usize,
    ) -> Self {
        let mut targets: Vec<ProTxHash> = data
            .members()
            .iter()
            .copied()
            .filter(|m| data.is_valid_member(m) && *m != pro_tx)
            .collect();
        targets.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        Self {
            quorum,
            targets,
            init_mask: data_mask,
            cur_mask: data_mask,
            pro_tx,
            start_offset,
            tries: 0,
            last_success: 0,
            current_member: None,
            phase: Phase::Init,
        }
    }

    /// Returns the quorum this job recovers data for.
    pub fn quorum(&self) -> QuorumRef {
        self.quorum
    }

    /// Returns the sorted valid quorum members other than `pro_tx`.
    pub fn targets(&self) -> &[ProTxHash] {
        &self.targets
    }

    /// Returns the remaining data mask (`0` once all requested fields have arrived).
    pub fn data_mask(&self) -> u16 {
        self.cur_mask
    }

    /// Returns the member-rotation start offset.
    pub fn start_offset(&self) -> usize {
        self.start_offset
    }

    /// Advances the job to its next sleep or completion.
    ///
    /// Prefer [`Self::poll_quorum`] when holding the [`QuorumData`], which releases the recovery
    /// claim once on entering [`Next::Done`] before `"Done"` is logged (`net_quorum.cpp:636-637`).
    // Core: v24.0.0 src/llmq/net_quorum.cpp:537-638 NetQuorum::DataRecoveryThread
    #[expect(clippy::too_many_arguments, reason = "pure poll takes each per-tick input explicitly")]
    pub fn poll(
        &mut self,
        now: i64,
        synced: bool,
        have_vvec: bool,
        have_sk: bool,
        interrupted: bool,
        peers: &[(PeerId, [u8; 32])],
        tracker: &mut Tracker,
    ) -> Poll {
        self.poll_inner(now, synced, have_vvec, have_sk, interrupted, peers, tracker, || {})
    }

    /// Polls against `quorum`'s live key presence and releases its recovery claim once on the
    /// transition into [`Next::Done`] (before the `"Done"` log line).
    // Core: v24.0.0 src/llmq/net_quorum.cpp:559-562 (Core returns before ReleaseRecovery; PLAN-QM1b F6 releases on purpose),575-584,636-637 NetQuorum::DataRecoveryThread
    pub fn poll_quorum<Q: QuorumData>(
        &mut self,
        now: i64,
        synced: bool,
        quorum: &Q,
        interrupted: bool,
        peers: &[(PeerId, [u8; 32])],
        tracker: &mut Tracker,
    ) -> Poll {
        let have_vvec = quorum.vvec().is_some();
        let have_sk = quorum.has_sk_share();
        self.poll_inner(now, synced, have_vvec, have_sk, interrupted, peers, tracker, || {
            quorum.release_recovery();
        })
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:537-638 NetQuorum::DataRecoveryThread
    #[expect(clippy::too_many_arguments, reason = "pure poll takes each per-tick input explicitly")]
    fn poll_inner(
        &mut self,
        now: i64,
        synced: bool,
        have_vvec: bool,
        have_sk: bool,
        interrupted: bool,
        peers: &[(PeerId, [u8; 32])],
        tracker: &mut Tracker,
        on_release: impl FnOnce(),
    ) -> Poll {
        let mut acts = Vec::new();
        let mut log = Vec::new();

        if self.phase == Phase::Finished {
            return Poll {
                acts,
                log,
                next: Next::Done,
            };
        }

        if self.phase == Phase::Init {
            log.push(self.format_log("Start"));
            self.phase = Phase::SyncWait;
        }

        if self.phase == Phase::SyncWait {
            if !synced && !interrupted {
                return Poll {
                    acts,
                    log,
                    next: Next::Sleep(SYNC_SLEEP),
                };
            }
            if interrupted {
                log.push(self.format_log("Aborted"));
                // Core: v24.0.0 src/llmq/net_quorum.cpp:559-562 NetQuorum::DataRecoveryThread
                // (Core returns before ReleaseRecovery; PLAN-QM1b F6 releases on purpose)
                on_release();
                self.phase = Phase::Finished;
                return Poll {
                    acts,
                    log,
                    next: Next::Done,
                };
            }
            log.push(self.format_log("Try to request"));
            self.phase = Phase::RequestLoop;
        }

        if self.phase == Phase::Connecting {
            self.last_success = now;
            if let Some(member) = self.current_member {
                acts.push(Act::AddPendingMasternode(member));
            }
            log.push(self.format_log("Connect"));
            self.for_each_peer(now, peers, tracker, &mut acts, &mut log);
            self.phase = Phase::RequestLoop;
            if !interrupted {
                return Poll {
                    acts,
                    log,
                    next: Next::Sleep(LOOP_SLEEP),
                };
            }
        }

        while self.cur_mask > 0 && !interrupted {
            if self.cur_mask & Request::QUORUM_VERIFICATION_VECTOR != 0 && have_vvec {
                self.cur_mask &= !Request::QUORUM_VERIFICATION_VECTOR;
                log.push(self.format_log("Received quorumVvec"));
            }

            if self.cur_mask & Request::ENCRYPTED_CONTRIBUTIONS != 0 && have_sk {
                self.cur_mask &= !Request::ENCRYPTED_CONTRIBUTIONS;
                log.push(self.format_log("Received skShare"));
            }

            if self.cur_mask == 0 {
                log.push(self.format_log("Success"));
                break;
            }

            if now.saturating_sub(self.last_success) > REQUEST_TIMEOUT {
                if self.tries >= self.targets.len() {
                    log.push(self.format_log("All tried but failed"));
                    break;
                }
                let len = self.targets.len();
                let idx = ((self.start_offset % len) + (self.tries % len)) % len;
                self.tries += 1;
                let member = self.targets[idx];
                self.current_member = Some(member);

                let key = Key {
                    pro_reg_tx: member,
                    we_requested: true,
                    quorum_hash: self.quorum.quorum_hash,
                    llmq_type: self.quorum.llmq_type,
                };
                if tracker.is_pending(&key, now) {
                    log.push(self.format_log("Already asked"));
                    continue;
                }

                self.phase = Phase::Connecting;
                let millis =
                    u64::try_from(self.start_offset).unwrap_or(u64::MAX).saturating_mul(100);
                return Poll {
                    acts,
                    log,
                    next: Next::Sleep(Duration::from_millis(millis)),
                };
            }

            self.for_each_peer(now, peers, tracker, &mut acts, &mut log);
            return Poll {
                acts,
                log,
                next: Next::Sleep(LOOP_SLEEP),
            };
        }

        // Core: v24.0.0 src/llmq/net_quorum.cpp:636-637 (ReleaseRecovery before Done)
        on_release();
        log.push(self.format_log("Done"));
        self.phase = Phase::Finished;
        Poll {
            acts,
            log,
            next: Next::Done,
        }
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:608-633 NetQuorum::DataRecoveryThread
    fn for_each_peer(
        &mut self,
        now: i64,
        peers: &[(PeerId, [u8; 32])],
        tracker: &mut Tracker,
        acts: &mut Vec<Act>,
        log: &mut Vec<String>,
    ) {
        for &(peer, verified) in peers {
            let Some(current) = self.current_member else {
                continue;
            };
            if verified != *current.as_bytes() {
                continue;
            }
            let req = Request {
                llmq_type: self.quorum.llmq_type,
                quorum_hash: self.quorum.quorum_hash,
                data_mask: self.cur_mask,
                pro_tx: self.pro_tx,
                error: Request::ERROR_UNDEFINED,
            };
            let outcome = tracker.request(current, req, now);
            self.apply_request_outcome(peer, current, now, outcome, acts, log);
        }
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:368-369,614-632 NetQuorum::RequestQuorumData / DataRecoveryThread
    fn apply_request_outcome(
        &mut self,
        peer: PeerId,
        member: ProTxHash,
        now: i64,
        outcome: Result<QuorumDataRequest, Status>,
        acts: &mut Vec<Act>,
        log: &mut Vec<String>,
    ) {
        match outcome {
            Ok(wire) => {
                self.last_success = now;
                log.push(format!(
                    "NetQuorum::RequestQuorumData -- sending QGETDATA quorumHash[{}] llmqType[{}] proRegTx[{member}]",
                    self.quorum.quorum_hash,
                    self.quorum.llmq_type.to_base(),
                ));
                acts.push(Act::Send(peer, Request::from(&wire)));
                log.push(self.format_log("Requested"));
            }
            Err(Status::NotFound) => {
                log.push(self.format_log("Failed"));
                acts.push(Act::Disconnect(peer));
                self.current_member = None;
            }
            Err(Status::Processed) => {
                log.push(self.format_log("Processed"));
                acts.push(Act::Disconnect(peer));
                self.current_member = None;
            }
            Err(Status::Pending) => {
                log.push(self.format_log("Waiting"));
            }
            Err(Status::Requested) => {
                unreachable!("Tracker::status never returns Requested (quorumsman.cpp:445-458)")
            }
        }
    }

    // Core: v24.0.0 src/llmq/net_quorum.cpp:547-552 NetQuorum::DataRecoveryThread printLog
    fn format_log(&self, msg: &str) -> String {
        let member = match self.current_member {
            None => "nullptr".to_owned(),
            Some(m) => m.to_string(),
        };
        format!(
            "NetQuorum::DataRecoveryThread -- {msg} - for llmqType {}, quorumHash {}, nDataMask ({}/{}), pCurrentMemberHash {member}, nTries {}",
            self.quorum.llmq_type.to_base(),
            self.quorum.quorum_hash,
            self.cur_mask,
            self.init_mask,
            self.tries,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::Arc;

    use super::*;
    use crate::qdata::fakes::{FakeQuorum, FakeQuorumState, FakeValidMns};
    use crate::qdata::tracker::ResponseCheck;
    use crate::quorum::decode_sk;

    const T0: i64 = 1_000_000;
    const VVEC: u16 = Request::QUORUM_VERIFICATION_VECTOR;
    const CONTRIB: u16 = Request::ENCRYPTED_CONTRIBUTIONS;
    const BOTH: u16 = VVEC | CONTRIB;

    /// Builds a `ProTxHash` whose raw byte 0 is `raw_head` and byte 31 (display head) is
    /// `display_head`, so raw memcmp and byte-reversed display order can diverge.
    fn pro_pair(raw_head: u8, display_head: u8) -> ProTxHash {
        let mut b = [0; 32];
        b[0] = raw_head;
        b[31] = display_head;
        ProTxHash::from(b)
    }

    fn pro(n: u8) -> ProTxHash {
        pro_pair(n, 0xff - n)
    }

    fn block(n: u8) -> BlockHash {
        let mut b = [0; 32];
        b[0] = n;
        b[31] = 0xa0 | n;
        BlockHash::from(b)
    }

    fn qref(llmq_type: LlmqType, n: u8) -> QuorumRef {
        QuorumRef {
            llmq_type,
            quorum_hash: block(n),
        }
    }

    fn tip(height: i32, n: u8) -> TipRef {
        TipRef {
            height,
            hash: block(n),
        }
    }

    fn make_quorum(
        members: Vec<ProTxHash>,
        valid: Vec<bool>,
        has_vvec: bool,
        has_sk: bool,
    ) -> FakeQuorum {
        let vvec = has_vvec.then(|| Arc::new(Vec::new()));
        let sk_share = has_sk.then(|| {
            let mut raw = [0; 32];
            raw[31] = 1;
            decode_sk(&raw).expect("valid scalar")
        });
        FakeQuorum::from(FakeQuorumState {
            threshold: 2,
            members,
            valid,
            vvec: RefCell::new(vvec),
            sk_share: RefCell::new(sk_share),
            accept_vvec: true,
            accept_sk_share: true,
            set_vvec_calls: RefCell::new(Vec::new()),
            set_sk_share_calls: RefCell::new(Vec::new()),
            recovery_claimed: Cell::new(false),
        })
    }

    fn expected_log(
        msg: &str,
        q: QuorumRef,
        cur_mask: u16,
        init_mask: u16,
        member: Option<ProTxHash>,
        tries: usize,
    ) -> String {
        let m = member.map_or_else(|| "nullptr".to_owned(), |p| p.to_string());
        format!(
            "NetQuorum::DataRecoveryThread -- {msg} - for llmqType {}, quorumHash {}, nDataMask ({cur_mask}/{init_mask}), pCurrentMemberHash {m}, nTries {tries}",
            q.llmq_type.to_base(),
            q.quorum_hash,
        )
    }

    fn expected_send_log(q: QuorumRef, member: ProTxHash) -> String {
        format!(
            "NetQuorum::RequestQuorumData -- sending QGETDATA quorumHash[{}] llmqType[{}] proRegTx[{member}]",
            q.quorum_hash,
            q.llmq_type.to_base(),
        )
    }

    #[test]
    fn plan_tip_gates_and_member_masks() {
        let me = pro(1);
        let m2 = pro(2);
        let m3 = pro(3);
        let trigger = tip(500, 9);

        let q_both = make_quorum(vec![me, m2, m3], vec![true, true, true], false, false);
        let q_need_sk = make_quorum(vec![me, m2, m3], vec![true, true, true], true, false);
        let q_need_vvec = make_quorum(vec![me, m2, m3], vec![true, true, true], false, true);
        let q_complete = make_quorum(vec![me, m2, m3], vec![true, true, true], true, true);
        let q_invalid_me = make_quorum(vec![me, m2, m3], vec![false, true, true], false, false);

        let scans = [(
            LlmqType::LlmqTest,
            vec![
                (block(1), q_both),
                (block(2), q_need_sk),
                (block(3), q_need_vvec),
                (block(4), q_complete),
                (block(5), q_invalid_me),
            ],
        )];
        let mn_role = Role::Masternode {
            pro_tx: Some(me),
            watching: false,
        };
        let unreg_role = Role::Masternode {
            pro_tx: None,
            watching: false,
        };
        let null_hex = "0".repeat(64);

        assert!(plan_tip(mn_role, false, &[], &scans, trigger).is_empty());
        assert!(plan_tip(Role::None, true, &[], &scans, trigger).is_empty());
        assert_eq!(process_block_log(mn_role, false, trigger.hash), None);
        assert_eq!(process_block_log(Role::Observer, false, trigger.hash), None);
        assert_eq!(process_block_log(Role::None, true, trigger.hash), None);
        assert_eq!(
            process_block_log(mn_role, true, trigger.hash),
            Some(format!(
                "NetQuorum::TriggerQuorumDataRecoveryThreads -- Process block {} as protx_hash={me}",
                trigger.hash
            ))
        );
        assert_eq!(
            process_block_log(unreg_role, true, trigger.hash),
            Some(format!(
                "NetQuorum::TriggerQuorumDataRecoveryThreads -- Process block {} as protx_hash={null_hex}",
                trigger.hash
            ))
        );
        assert_eq!(
            process_block_log(Role::Observer, true, trigger.hash),
            Some(format!(
                "NetQuorum::TriggerQuorumDataRecoveryThreads -- Process block {} as protx_hash={null_hex}",
                trigger.hash
            ))
        );

        let plans = plan_tip(mn_role, true, &[], &scans, trigger);
        assert_eq!(
            plans,
            vec![
                Plan::SkShare {
                    q: qref(LlmqType::LlmqTest, 1),
                    mask: BOTH,
                    trigger,
                },
                Plan::SkShare {
                    q: qref(LlmqType::LlmqTest, 2),
                    mask: CONTRIB,
                    trigger,
                },
                Plan::SkShare {
                    q: qref(LlmqType::LlmqTest, 3),
                    mask: VVEC,
                    trigger,
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 4),
                    caller: "TriggerQuorumDataRecoveryThreads",
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 5),
                    caller: "TryStartVvecSyncThread",
                },
            ]
        );
        assert_eq!(plans[0].no_data_needed_log(trigger.height), None);
        assert_eq!(
            plans[3].no_data_needed_log(trigger.height),
            Some(format!(
                "NetQuorum::TriggerQuorumDataRecoveryThreads -- No data needed from (100, {}) at height 500",
                block(4)
            ))
        );
        assert_eq!(
            plans[4].no_data_needed_log(trigger.height),
            Some(format!(
                "NetQuorum::TryStartVvecSyncThread -- No data needed from (100, {}) at height 500",
                block(5)
            ))
        );

        let unreg_always =
            plan_tip(unreg_role, true, &[(LlmqType::LlmqTest, SyncMode::Always)], &scans, trigger);
        assert_eq!(
            unreg_always,
            vec![
                Plan::VvecSync {
                    q: qref(LlmqType::LlmqTest, 1),
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 2),
                    caller: "TryStartVvecSyncThread",
                },
                Plan::VvecSync {
                    q: qref(LlmqType::LlmqTest, 3),
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 4),
                    caller: "TryStartVvecSyncThread",
                },
                Plan::VvecSync {
                    q: qref(LlmqType::LlmqTest, 5),
                },
            ]
        );
        assert_eq!(unreg_always[0].no_data_needed_log(trigger.height), None);
    }

    #[test]
    fn plan_tip_vvec_sync_modes_and_scanned_type_membership() {
        let me = pro(1);
        let m2 = pro(2);
        let m3 = pro(3);
        let trigger = tip(600, 10);
        let mn_role = Role::Masternode {
            pro_tx: Some(me),
            watching: false,
        };

        // Mode 0 (`Always`) on an observer: skips a quorum that already has its vvec and omits a
        // quorum whose recovery claim is already running.
        let q_missing = make_quorum(vec![m2, m3], vec![true, true], false, false);
        let q_have_vvec = make_quorum(vec![m2, m3], vec![true, true], true, false);
        let q_running = make_quorum(vec![m2, m3], vec![true, true], false, false);
        assert!(q_running.try_claim_recovery());

        let always_scans = [(
            LlmqType::LlmqTest,
            vec![(block(1), q_missing), (block(2), q_have_vvec), (block(3), q_running)],
        )];
        assert_eq!(
            plan_tip(
                Role::Observer,
                true,
                &[(LlmqType::LlmqTest, SyncMode::Always)],
                &always_scans,
                trigger,
            ),
            vec![
                Plan::VvecSync {
                    q: qref(LlmqType::LlmqTest, 1),
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 2),
                    caller: "TryStartVvecSyncThread",
                },
            ]
        );

        // Mode 1 (`OnlyIfTypeMember`): `me` is NOT a member of the newest quorum (`block(11)`),
        // IS a valid member of an older scanned quorum of the same type (`block(13)`), and is not
        // in any scanned quorum of `Llmq50_60`. `block(12)` already has its vvec and must be
        // skipped.
        let q_newest_not_me = make_quorum(vec![m2, m3], vec![true, true], false, false);
        let q_older_has_vvec = make_quorum(vec![m2, m3], vec![true, true], true, false);
        let q_oldest_me_valid = make_quorum(vec![me, m2], vec![true, true], true, true);
        let q_other_type = make_quorum(vec![m2, m3], vec![true, true], false, false);

        let mode1_scans = [
            (
                LlmqType::LlmqTest,
                vec![
                    (block(11), q_newest_not_me.clone()),
                    (block(12), q_older_has_vvec),
                    (block(13), q_oldest_me_valid),
                ],
            ),
            (LlmqType::Llmq50_60, vec![(block(21), q_other_type)]),
        ];
        let sync = [
            (LlmqType::LlmqTest, SyncMode::OnlyIfTypeMember),
            (LlmqType::Llmq50_60, SyncMode::OnlyIfTypeMember),
        ];

        let mode1_plans = plan_tip(mn_role, true, &sync, &mode1_scans, trigger);
        assert_eq!(
            mode1_plans,
            vec![
                Plan::VvecSync {
                    q: qref(LlmqType::LlmqTest, 11),
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 12),
                    caller: "TryStartVvecSyncThread",
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 13),
                    caller: "TriggerQuorumDataRecoveryThreads",
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::Llmq50_60, 21),
                    caller: "TryStartVvecSyncThread",
                },
            ]
        );
        assert_eq!(
            mode1_plans[3].no_data_needed_log(trigger.height),
            Some(format!(
                "NetQuorum::TryStartVvecSyncThread -- No data needed from (1, {}) at height 600",
                block(21)
            ))
        );

        // Observer in Mode 1 is never a quorum member.
        assert_eq!(
            plan_tip(Role::Observer, true, &sync, &mode1_scans, trigger),
            vec![
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 11),
                    caller: "TryStartVvecSyncThread",
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 12),
                    caller: "TryStartVvecSyncThread",
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 13),
                    caller: "TryStartVvecSyncThread",
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::Llmq50_60, 21),
                    caller: "TryStartVvecSyncThread",
                },
            ]
        );

        // Mode 1 negative: `me` is only an INVALID member of an older quorum (`valid = false`).
        let q_oldest_me_invalid = make_quorum(vec![me, m2], vec![false, true], false, false);
        let negative_scans = [(
            LlmqType::LlmqTest,
            vec![(block(11), q_newest_not_me), (block(14), q_oldest_me_invalid)],
        )];
        assert_eq!(
            plan_tip(mn_role, true, &sync, &negative_scans, trigger),
            vec![
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 11),
                    caller: "TryStartVvecSyncThread",
                },
                Plan::NoDataNeeded {
                    q: qref(LlmqType::LlmqTest, 14),
                    caller: "TryStartVvecSyncThread",
                },
            ]
        );
    }

    #[test]
    fn claim_and_start_jobs() {
        let me = pro(4);
        let m1 = pro(1);
        let m2 = pro(2);
        let trigger = tip(700, 15);
        let q = qref(LlmqType::LlmqTest, 1);

        let empty_quorum = make_quorum(Vec::new(), Vec::new(), false, false);
        assert_eq!(
            Plan::NoDataNeeded {
                q,
                caller: "TryStartVvecSyncThread",
            }
            .claim(&empty_quorum),
            Claim::Skip
        );
        assert_eq!(
            Plan::VvecSync {
                q
            }
            .claim(&empty_quorum),
            Claim::Skip
        );
        assert_eq!(
            Plan::SkShare {
                q,
                mask: BOTH,
                trigger,
            }
            .claim(&empty_quorum),
            Claim::Skip
        );
        assert!(!empty_quorum.recovery_running());

        let quorum = make_quorum(vec![m2, me, m1], vec![true, true, true], false, false);
        let Claim::Claimed(claimed_vvec) = Plan::VvecSync {
            q,
        }
        .claim(&quorum) else {
            panic!("expected Claimed");
        };
        assert!(quorum.recovery_running());
        assert_eq!(
            Plan::VvecSync {
                q
            }
            .claim(&quorum),
            Claim::AlreadyRunning("NetQuorum::StartVvecSyncThread -- Already running")
        );
        assert_eq!(
            Plan::SkShare {
                q,
                mask: BOTH,
                trigger,
            }
            .claim(&quorum),
            Claim::AlreadyRunning("NetQuorum::StartSkShareRecoveryThread -- Already running")
        );

        let empty_mns = FakeValidMns::default();
        let vvec_job = claimed_vvec.start(&quorum, &empty_mns, me).expect("vvec needs no MN list");
        assert_eq!(vvec_job.start_offset(), 0);
        assert_eq!(vvec_job.data_mask(), VVEC);
        assert_eq!(vvec_job.targets(), &[m1, m2, me]);
        quorum.release_recovery();

        // Unavailable `ValidMnsAt` at trigger block releases the claim and starts no job.
        let Claim::Claimed(claimed_sk_missing) = (Plan::SkShare {
            q,
            mask: BOTH,
            trigger,
        })
        .claim(&quorum) else {
            panic!("expected Claimed");
        };
        assert!(claimed_sk_missing.start(&quorum, &empty_mns, me).is_none());
        assert!(!quorum.recovery_running());

        // Queued job dropped before starting releases its claim.
        let Claim::Claimed(claimed_drop) = Plan::VvecSync {
            q,
        }
        .claim(&quorum) else {
            panic!("expected Claimed");
        };
        claimed_drop.release(&quorum);
        assert!(!quorum.recovery_running());

        // Available `ValidMnsAt` computes `start_offset` at job start.
        let mut valid_mns = FakeValidMns::default();
        valid_mns.lists.insert(trigger.hash, vec![pro(5), me, m2, pro(0), m1, pro(3)]);
        let Claim::Claimed(claimed_sk) = (Plan::SkShare {
            q,
            mask: BOTH,
            trigger,
        })
        .claim(&quorum) else {
            panic!("expected Claimed");
        };
        let sk_job = claimed_sk.start(&quorum, &valid_mns, me).expect("MN list present");
        assert_eq!(sk_job.start_offset(), 1);
        assert_eq!(sk_job.data_mask(), BOTH);
        assert_eq!(sk_job.targets(), &[m1, m2]);
    }

    #[test]
    fn start_offset_raw_memcmp_invalid_members_and_extra_enabled_mns() {
        // Construct 8 enabled MNs whose raw byte 0 (`0x01..=0x08`) is strictly opposite to byte 31
        // (`0xf0..=0x80`, the leading byte of Core's reversed hex `ToString()`).
        let h1 = pro_pair(0x01, 0xf0);
        let h2 = pro_pair(0x02, 0xe0);
        let h3 = pro_pair(0x03, 0xd0);
        let h4 = pro_pair(0x04, 0xc0);
        let h5 = pro_pair(0x05, 0xb0);
        let h6 = pro_pair(0x06, 0xa0);
        let h7 = pro_pair(0x07, 0x90);
        let h8 = pro_pair(0x08, 0x80);

        // Shuffled enabled list at trigger block, containing 3 extra MNs outside the 5-member
        // quorum (`h2`, `h6`, `h8`).
        let enabled = [h6, h2, h8, h1, h5, h3, h7, h4];

        // Quorum has 5 members (`quorum_size = 5`), 2 of which are invalid (`h3`, `h7`), leaving
        // `valid_count = 3` (`h1`, `h4`, `h5`) and `targets.len() = 2` for `me = h4`.
        let quorum = make_quorum(
            vec![h5, h3, h1, h7, h4],
            vec![true, false, true, false, true],
            false,
            false,
        );
        assert_eq!(quorum.size(), 5);
        assert_eq!(quorum.valid_count(), 3);

        // Raw memcmp order: [h1, h2, h3, h4, h5, h6, h7, h8].
        // Display (reversed) order: [h8, h7, h6, h5, h4, h3, h2, h1].
        // For `h4`: raw index is 3 -> 3 % 5 == 3 (display index is 4 -> 4 % 5 == 4; `3 % 3` would
        // be 0; `3 % 2` would be 1).
        assert_eq!(start_offset(&enabled, &h4, quorum.size()), 3);
        // For `h6`: raw index is 5 -> 5 % 5 == 0 (display index is 2 -> 2 % 5 == 2).
        assert_eq!(start_offset(&enabled, &h6, quorum.size()), 0);
        // For `h7`: raw index is 6 -> 6 % 5 == 1 (display index is 1 -> 1 % 5 == 1, check `h8`:
        // raw index 7 -> 7 % 5 == 2, display index 0 -> 0 % 5 == 0).
        assert_eq!(start_offset(&enabled, &h8, quorum.size()), 2);

        // Absent `me` (e.g. PoSe-banned by trigger height) defaults to index 0.
        let banned_me = pro_pair(0x99, 0x01);
        assert_eq!(start_offset(&enabled, &banned_me, quorum.size()), 0);

        // Zero `quorum_size` returns 0 without dividing by zero.
        assert_eq!(start_offset(&enabled, &h4, 0), 0);

        // `RecoveryJob::new` keeps only valid members != `me` and sorts them by raw memcmp
        // (`[h1, h5]`, whereas display order would be `[h5, h1]`).
        let job = RecoveryJob::new(qref(LlmqType::LlmqTest, 1), &quorum, BOTH, h4, 3);
        assert_eq!(job.targets(), &[h1, h5]);
    }

    #[test]
    fn trace_sync_changes_and_data_arriving() {
        let me = pro(1);
        let m2 = pro(2);
        let m3 = pro(3);
        let q = qref(LlmqType::LlmqTest, 1);
        let quorum = make_quorum(vec![m3, me, m2], vec![true, true, true], false, false);
        assert!(quorum.try_claim_recovery());

        let mut job = RecoveryJob::new(q, &quorum, BOTH, me, 3);
        let mut tracker = Tracker::new();
        // `start_offset = 3`, `targets = [m2, m3]`: `(3 + 0) % 2 == 1` -> `m3` first!
        let peers = [(10, *m3.as_bytes())];

        // Tick 1 (T0, unsynced): logs "Start" once and sleeps 10 s.
        let p1 = job.poll_quorum(T0, false, &quorum, false, &peers, &mut tracker);
        assert_eq!(
            p1,
            Poll {
                acts: Vec::new(),
                log: vec![expected_log("Start", q, BOTH, BOTH, None, 0)],
                next: Next::Sleep(Duration::from_secs(10)),
            }
        );
        assert!(quorum.recovery_running());

        // Tick 2 (T0 + 10, still unsynced): no duplicate "Start", sleeps another 10 s.
        let p2 = job.poll_quorum(T0 + 10, false, &quorum, false, &peers, &mut tracker);
        assert_eq!(
            p2,
            Poll {
                acts: Vec::new(),
                log: Vec::new(),
                next: Next::Sleep(Duration::from_secs(10)),
            }
        );

        // Tick 3 (T0 + 20, synced): logs "Try to request", picks `m3`, sleeps `3 * 100 ms`.
        let p3 = job.poll_quorum(T0 + 20, true, &quorum, false, &peers, &mut tracker);
        assert_eq!(
            p3,
            Poll {
                acts: Vec::new(),
                log: vec![expected_log("Try to request", q, BOTH, BOTH, None, 0)],
                next: Next::Sleep(Duration::from_millis(300)),
            }
        );

        // Tick 4 (T0 + 20 after 300 ms; even if `synced` flips back to false, request loop
        // continues as Core does): emits `AddPendingMasternode(m3)` + `Send(10, req)`, logs
        // "Connect", "sending QGETDATA" (with peer `m3`, not `me`) and "Requested", sleeps 1 s.
        let p4 = job.poll_quorum(T0 + 20, false, &quorum, false, &peers, &mut tracker);
        let expected_req = Request {
            llmq_type: q.llmq_type,
            quorum_hash: q.quorum_hash,
            data_mask: BOTH,
            pro_tx: me,
            error: Request::ERROR_UNDEFINED,
        };
        assert_eq!(
            p4,
            Poll {
                acts: vec![Act::AddPendingMasternode(m3), Act::Send(10, expected_req),],
                log: vec![
                    expected_log("Connect", q, BOTH, BOTH, Some(m3), 1),
                    format!(
                        "NetQuorum::RequestQuorumData -- sending QGETDATA quorumHash[{}] llmqType[100] proRegTx[{m3}]",
                        q.quorum_hash
                    ),
                    expected_log("Requested", q, BOTH, BOTH, Some(m3), 1),
                ],
                next: Next::Sleep(Duration::from_secs(1)),
            }
        );

        // Tick 5 (T0 + 21, awaiting response): logs "Waiting".
        let p5 = job.poll_quorum(T0 + 21, true, &quorum, false, &peers, &mut tracker);
        assert_eq!(
            p5,
            Poll {
                acts: Vec::new(),
                log: vec![expected_log("Waiting", q, BOTH, BOTH, Some(m3), 1)],
                next: Next::Sleep(Duration::from_secs(1)),
            }
        );

        // Tick 6 (T0 + 22, vvec arrives first): clears VVEC bit (mask 3 -> 2), logs
        // "Received quorumVvec" then "Waiting".
        *quorum.state().vvec.borrow_mut() = Some(Arc::new(Vec::new()));
        let p6 = job.poll_quorum(T0 + 22, true, &quorum, false, &peers, &mut tracker);
        assert_eq!(
            p6,
            Poll {
                acts: Vec::new(),
                log: vec![
                    expected_log("Received quorumVvec", q, CONTRIB, BOTH, Some(m3), 1),
                    expected_log("Waiting", q, CONTRIB, BOTH, Some(m3), 1),
                ],
                next: Next::Sleep(Duration::from_secs(1)),
            }
        );
        assert!(quorum.recovery_running());

        // Tick 7 (T0 + 23, sk_share arrives): clears CONTRIB bit (mask 2 -> 0), logs
        // "Received skShare", "Success", "Done", returns `Done` and releases recovery claim.
        let mut raw_sk = [0; 32];
        raw_sk[31] = 1;
        *quorum.state().sk_share.borrow_mut() = Some(decode_sk(&raw_sk).expect("valid scalar"));
        let p7 = job.poll_quorum(T0 + 23, true, &quorum, false, &peers, &mut tracker);
        assert_eq!(
            p7,
            Poll {
                acts: Vec::new(),
                log: vec![
                    expected_log("Received skShare", q, 0, BOTH, Some(m3), 1),
                    expected_log("Success", q, 0, BOTH, Some(m3), 1),
                    expected_log("Done", q, 0, BOTH, Some(m3), 1),
                ],
                next: Next::Done,
            }
        );
        assert!(!quorum.recovery_running());
    }

    #[test]
    fn trace_retarget_boundary_10_vs_11_seconds_and_exhaustion() {
        let m1 = pro(1);
        let m2 = pro(2);
        let q = qref(LlmqType::LlmqTest, 2);
        let quorum = make_quorum(vec![m1, m2], vec![true, true], false, false);
        let mut job = RecoveryJob::new(q, &quorum, VVEC, ProTxHash::default(), 2);
        let mut tracker = Tracker::new();

        // T0: initial poll selects `m1` (`(2 + 0) % 2 == 0`), sleeps 200 ms.
        let p1 = job.poll(T0, true, false, false, false, &[], &mut tracker);
        assert_eq!(p1.next, Next::Sleep(Duration::from_millis(200)));

        // T0 + 1 (after 200 ms crosses a second boundary, no peers yet): `Connect` sets
        // `last_success = T0 + 1` (post-sleep `now`, `net_quorum.cpp:603`).
        let p2 = job.poll(T0 + 1, true, false, false, false, &[], &mut tracker);
        assert_eq!(p2.acts, vec![Act::AddPendingMasternode(m1)]);
        assert_eq!(p2.log, vec![expected_log("Connect", q, VVEC, VVEC, Some(m1), 1)]);
        assert_eq!(p2.next, Next::Sleep(Duration::from_secs(1)));

        // T0 + 11 (`(T0 + 11) - (T0 + 1) == 10 <= 10`, whereas from retarget `T0` it would be 11):
        // does NOT retarget, and `m1` connecting on peer 7 sends `qgetdata` and refreshes
        // `last_success = T0 + 11`.
        let peers_m1 = [(7, *m1.as_bytes())];
        let p3 = job.poll(T0 + 11, true, false, false, false, &peers_m1, &mut tracker);
        assert_eq!(p3.acts.len(), 1);
        assert_eq!(
            p3.log,
            vec![expected_send_log(q, m1), expected_log("Requested", q, VVEC, VVEC, Some(m1), 1),]
        );

        // Exact 10 s boundary (`(T0 + 21) - (T0 + 11) == 10`): strict `> 10` does NOT retarget!
        let p4 = job.poll(T0 + 21, true, false, false, false, &peers_m1, &mut tracker);
        assert_eq!(
            p4,
            Poll {
                acts: Vec::new(),
                log: vec![expected_log("Waiting", q, VVEC, VVEC, Some(m1), 1)],
                next: Next::Sleep(Duration::from_secs(1)),
            }
        );

        // Exact 11 s boundary (`(T0 + 22) - (T0 + 11) == 11 > 10`): retargets to `m2`!
        let p5 = job.poll(T0 + 22, true, false, false, false, &peers_m1, &mut tracker);
        assert_eq!(
            p5,
            Poll {
                acts: Vec::new(),
                log: Vec::new(),
                next: Next::Sleep(Duration::from_millis(200)),
            }
        );

        // T0 + 23 (200 ms sleep crosses into the next second): `Connect` to `m2` sets
        // `last_success = T0 + 23` (post-sleep `now`, not `T0 + 22`).
        let p6 = job.poll(T0 + 23, true, false, false, false, &peers_m1, &mut tracker);
        assert_eq!(
            p6,
            Poll {
                acts: vec![Act::AddPendingMasternode(m2)],
                log: vec![expected_log("Connect", q, VVEC, VVEC, Some(m2), 2)],
                next: Next::Sleep(Duration::from_secs(1)),
            }
        );

        // T0 + 33 (`(T0 + 33) - (T0 + 23) == 10 <= 10`, `m2` never connected): still waiting on
        // `m2` without retargeting (would fail if `last_success` were set at `p5`'s `T0 + 22`).
        let p7 = job.poll(T0 + 33, true, false, false, false, &peers_m1, &mut tracker);
        assert_eq!(
            p7,
            Poll {
                acts: Vec::new(),
                log: Vec::new(),
                next: Next::Sleep(Duration::from_secs(1)),
            }
        );

        // T0 + 34 (`(T0 + 34) - (T0 + 23) == 11 > 10`, `tries == 2 == targets.len()`): exhausts
        // targets and finishes.
        let p8 = job.poll(T0 + 34, true, false, false, false, &peers_m1, &mut tracker);
        assert_eq!(
            p8,
            Poll {
                acts: Vec::new(),
                log: vec![
                    expected_log("All tried but failed", q, VVEC, VVEC, Some(m2), 2),
                    expected_log("Done", q, VVEC, VVEC, Some(m2), 2),
                ],
                next: Next::Done,
            }
        );
    }

    #[test]
    fn trace_peer_order_duplicate_connections_and_disconnect_outcomes() {
        let m1 = pro(1);
        let m2 = pro(2);
        let outsider = pro(9);
        let q = qref(LlmqType::LlmqTest, 3);
        let quorum = make_quorum(vec![m1, m2], vec![true, true], false, false);

        // Sub-trace A: duplicate connections `(21, m1)` then `(22, m1)` after unverified and
        // unrelated peers — peer 21 gets the single `Send`, peer 22 logs "Waiting".
        let mut job = RecoveryJob::new(q, &quorum, VVEC, ProTxHash::default(), 0);
        let mut tracker = Tracker::new();
        let peers_a =
            [(19, [0; 32]), (20, *outsider.as_bytes()), (21, *m1.as_bytes()), (22, *m1.as_bytes())];

        assert_eq!(
            job.poll(T0, true, false, false, false, &peers_a, &mut tracker).next,
            Next::Sleep(Duration::ZERO)
        );

        let req = Request {
            llmq_type: q.llmq_type,
            quorum_hash: q.quorum_hash,
            data_mask: VVEC,
            pro_tx: ProTxHash::default(),
            error: Request::ERROR_UNDEFINED,
        };
        let step_send = job.poll(T0, true, false, false, false, &peers_a, &mut tracker);
        assert_eq!(
            step_send,
            Poll {
                acts: vec![Act::AddPendingMasternode(m1), Act::Send(21, req),],
                log: vec![
                    expected_log("Connect", q, VVEC, VVEC, Some(m1), 1),
                    expected_send_log(q, m1),
                    expected_log("Requested", q, VVEC, VVEC, Some(m1), 1),
                    expected_log("Waiting", q, VVEC, VVEC, Some(m1), 1),
                ],
                next: Next::Sleep(Duration::from_secs(1)),
            }
        );

        // Sub-trace B: reversing the duplicate peer order (`22` before `21`) sends to `22`.
        let mut job_rev = RecoveryJob::new(q, &quorum, VVEC, ProTxHash::default(), 0);
        let mut tracker_rev = Tracker::new();
        let peers_b = [(22, *m1.as_bytes()), (21, *m1.as_bytes())];
        job_rev.poll(T0, true, false, false, false, &peers_b, &mut tracker_rev);
        let step_rev = job_rev.poll(T0, true, false, false, false, &peers_b, &mut tracker_rev);
        assert_eq!(step_rev.acts, vec![Act::AddPendingMasternode(m1), Act::Send(22, req)]);
        assert_eq!(
            step_rev.log,
            vec![
                expected_log("Connect", q, VVEC, VVEC, Some(m1), 1),
                expected_send_log(q, m1),
                expected_log("Requested", q, VVEC, VVEC, Some(m1), 1),
                expected_log("Waiting", q, VVEC, VVEC, Some(m1), 1),
            ]
        );

        // Mark `m1`'s request processed (e.g. an error `qdata` where `have_vvec` stays false):
        // next poll logs "Processed" with `m1` BEFORE clearing `current_member`, disconnects
        // peer 21, and skips peer 22 because `current_member` is now `None`.
        let key_m1 = Key {
            pro_reg_tx: m1,
            we_requested: true,
            quorum_hash: q.quorum_hash,
            llmq_type: q.llmq_type,
        };
        assert_eq!(tracker.validate_response(&key_m1, &req), ResponseCheck::Ok);
        let step_proc = job.poll(T0 + 1, true, false, false, false, &peers_a, &mut tracker);
        assert_eq!(
            step_proc,
            Poll {
                acts: vec![Act::Disconnect(21)],
                log: vec![expected_log("Processed", q, VVEC, VVEC, Some(m1), 1)],
                next: Next::Sleep(Duration::from_secs(1)),
            }
        );
        // Subsequent poll while `current_member` is `None` (before 11 s retarget) emits nothing.
        let step_idle = job.poll(T0 + 2, true, false, false, false, &peers_a, &mut tracker);
        assert_eq!(
            step_idle,
            Poll {
                acts: Vec::new(),
                log: Vec::new(),
                next: Next::Sleep(Duration::from_secs(1)),
            }
        );

        // Direct check of `Status::NotFound` ("Failed" branch): logs "Failed" with the current
        // member hash before clearing `current_member`, and disconnects the peer.
        job.current_member = Some(m1);
        let mut acts = Vec::new();
        let mut log = Vec::new();
        job.apply_request_outcome(21, m1, T0 + 3, Err(Status::NotFound), &mut acts, &mut log);
        assert_eq!(acts, vec![Act::Disconnect(21)]);
        assert_eq!(log, vec![expected_log("Failed", q, VVEC, VVEC, Some(m1), 1)]);
        assert_eq!(job.current_member, None);
    }

    #[test]
    fn trace_already_asked_and_cleanup_between_polls() {
        let m1 = pro(1);
        let m2 = pro(2);
        let m3 = pro(3);
        let q = qref(LlmqType::LlmqTest, 4);
        let quorum = make_quorum(vec![m1, m2, m3], vec![true, true, true], false, false);
        let req = Request {
            llmq_type: q.llmq_type,
            quorum_hash: q.quorum_hash,
            data_mask: VVEC,
            pro_tx: ProTxHash::default(),
            error: Request::ERROR_UNDEFINED,
        };
        let key_m1 = Key {
            pro_reg_tx: m1,
            we_requested: true,
            quorum_hash: q.quorum_hash,
            llmq_type: q.llmq_type,
        };
        let key_m3 = Key {
            pro_reg_tx: m3,
            we_requested: true,
            quorum_hash: q.quorum_hash,
            llmq_type: q.llmq_type,
        };

        let mut tracker = Tracker::new();
        // `m1` asked at `T0`, `m3` asked at `T0 + 11` (biased expiry = +360 s).
        tracker.request(m1, req, T0).expect("registered m1");
        tracker.request(m3, req, T0 + 11).expect("registered m3");

        let mut job = RecoveryJob::new(q, &quorum, VVEC, ProTxHash::default(), 0);

        // At `T0 + 359`, `m1` is still pending (359 < 360) AND connected on peer 31: `poll` logs
        // "Already asked" for `m1` (`nTries 1`), `continue`s WITHOUT running `for_each_peer` (so
        // peer 31 produces no "Waiting" or `Send`), and advances in the same poll to `m2`
        // (`nTries 2`).
        let peers_m1 = [(31, *m1.as_bytes())];
        let p1 = job.poll(T0 + 359, true, false, false, false, &peers_m1, &mut tracker);
        assert_eq!(
            p1,
            Poll {
                acts: Vec::new(),
                log: vec![
                    expected_log("Start", q, VVEC, VVEC, None, 0),
                    expected_log("Try to request", q, VVEC, VVEC, None, 0),
                    expected_log("Already asked", q, VVEC, VVEC, Some(m1), 1),
                ],
                next: Next::Sleep(Duration::ZERO),
            }
        );

        // `Connect` to `m2` at `T0 + 359` (`last_success = T0 + 359`).
        let p2 = job.poll(T0 + 359, true, false, false, false, &peers_m1, &mut tracker);
        assert_eq!(p2.acts, vec![Act::AddPendingMasternode(m2)]);

        // Between polls at `T0 + 371`, `cleanup(T0 + 371)` expires both `m1` (age 371 >= 360) and
        // `m3` (age 360 >= 360). Retargeting at `T0 + 371` (`371 - 359 = 12 > 10`) selects `m3`
        // (`nTries 3`), which is no longer pending, connects, and sends `qgetdata`.
        assert!(tracker.is_pending(&key_m3, T0 + 370));
        tracker.cleanup(T0 + 371);
        assert_eq!(tracker.status(&key_m1), Status::NotFound);
        assert_eq!(tracker.status(&key_m3), Status::NotFound);

        let peers_m3 = [(33, *m3.as_bytes())];
        let p3 = job.poll(T0 + 371, true, false, false, false, &peers_m3, &mut tracker);
        assert_eq!(p3.next, Next::Sleep(Duration::ZERO));

        let p4 = job.poll(T0 + 371, true, false, false, false, &peers_m3, &mut tracker);
        assert_eq!(
            p4,
            Poll {
                acts: vec![Act::AddPendingMasternode(m3), Act::Send(33, req),],
                log: vec![
                    expected_log("Connect", q, VVEC, VVEC, Some(m3), 3),
                    expected_send_log(q, m3),
                    expected_log("Requested", q, VVEC, VVEC, Some(m3), 3),
                ],
                next: Next::Sleep(Duration::from_secs(1)),
            }
        );
    }

    #[test]
    fn interruption_during_sync_wait_vs_inside_request_loop() {
        let m1 = pro(1);
        let q = qref(LlmqType::LlmqTest, 5);
        let quorum = make_quorum(vec![m1], vec![true], false, false);

        // 1a. Interrupted during initial sync wait: logs "Aborted" (no "Done"), returns `Done`,
        // and `poll_quorum` releases the claim.
        assert!(quorum.try_claim_recovery());
        let mut job_sync = RecoveryJob::new(q, &quorum, VVEC, ProTxHash::default(), 0);
        let mut tracker = Tracker::new();
        let s1 = job_sync.poll_quorum(T0, false, &quorum, false, &[], &mut tracker);
        assert_eq!(s1.next, Next::Sleep(Duration::from_secs(10)));
        assert!(quorum.recovery_running());

        let s2 = job_sync.poll_quorum(T0 + 3, false, &quorum, true, &[], &mut tracker);
        assert_eq!(
            s2,
            Poll {
                acts: Vec::new(),
                log: vec![expected_log("Aborted", q, VVEC, VVEC, None, 0)],
                next: Next::Done,
            }
        );
        assert!(!quorum.recovery_running());

        // 1b. Interrupted already on the very first poll (even when `synced == true`): logs
        // "Start" then "Aborted", never "Done".
        let mut job_immediate = RecoveryJob::new(q, &quorum, VVEC, ProTxHash::default(), 0);
        let s_imm = job_immediate.poll(T0, true, false, false, true, &[], &mut tracker);
        assert_eq!(
            s_imm,
            Poll {
                acts: Vec::new(),
                log: vec![
                    expected_log("Start", q, VVEC, VVEC, None, 0),
                    expected_log("Aborted", q, VVEC, VVEC, None, 0),
                ],
                next: Next::Done,
            }
        );

        // 2a. Interrupted inside the request loop after the 1 s sleep: logs "Done" with no
        // "Aborted", returns `Done`, and releases the claim.
        assert!(quorum.try_claim_recovery());
        let mut job_loop = RecoveryJob::new(q, &quorum, VVEC, ProTxHash::default(), 1);
        assert_eq!(
            job_loop.poll_quorum(T0, true, &quorum, false, &[], &mut tracker).next,
            Next::Sleep(Duration::from_millis(100))
        );
        assert_eq!(
            job_loop.poll_quorum(T0, true, &quorum, false, &[], &mut tracker).next,
            Next::Sleep(Duration::from_secs(1))
        );
        let l3 = job_loop.poll_quorum(T0 + 1, true, &quorum, true, &[], &mut tracker);
        assert_eq!(
            l3,
            Poll {
                acts: Vec::new(),
                log: vec![expected_log("Done", q, VVEC, VVEC, Some(m1), 1)],
                next: Next::Done,
            }
        );
        assert!(!l3.log.iter().any(|line| line.contains("Aborted")));
        assert!(!quorum.recovery_running());

        // 2b. Interrupted during the pre-connect `offset * 100 ms` sleep with `m1` connected on
        // peer 51: Core still runs `AddPendingMasternode`, "Connect", and `ForEachNode` (sending
        // `qgetdata` and logging "sending QGETDATA" + "Requested"), then skips the 1 s sleep on
        // the interrupt, releases the claim, and logs "Done" (no "Aborted").
        assert!(quorum.try_claim_recovery());
        let mut job_conn = RecoveryJob::new(q, &quorum, VVEC, ProTxHash::default(), 2);
        let peers_m1 = [(51, *m1.as_bytes())];
        let expected_req = Request {
            llmq_type: q.llmq_type,
            quorum_hash: q.quorum_hash,
            data_mask: VVEC,
            pro_tx: ProTxHash::default(),
            error: Request::ERROR_UNDEFINED,
        };
        assert_eq!(
            job_conn.poll_quorum(T0, true, &quorum, false, &peers_m1, &mut tracker).next,
            Next::Sleep(Duration::from_millis(200))
        );
        let c2 = job_conn.poll_quorum(T0, true, &quorum, true, &peers_m1, &mut tracker);
        assert_eq!(
            c2,
            Poll {
                acts: vec![Act::AddPendingMasternode(m1), Act::Send(51, expected_req)],
                log: vec![
                    expected_log("Connect", q, VVEC, VVEC, Some(m1), 1),
                    expected_send_log(q, m1),
                    expected_log("Requested", q, VVEC, VVEC, Some(m1), 1),
                    expected_log("Done", q, VVEC, VVEC, Some(m1), 1),
                ],
                next: Next::Done,
            }
        );
        assert!(!c2.log.iter().any(|line| line.contains("Aborted")));
        assert!(!quorum.recovery_running());
    }

    #[test]
    fn repoll_after_done_leaves_next_claim_set() {
        let me = pro(1);
        let m2 = pro(2);
        let q = qref(LlmqType::LlmqTest, 6);
        let trigger = tip(800, 16);
        let empty_mns = FakeValidMns::default();
        let quorum = make_quorum(vec![me, m2], vec![true, true], true, false);
        let mut tracker = Tracker::new();

        // Job A claims `quorum`, starts, sees vvec already present, and finishes on `poll_quorum`
        // (releasing its claim once).
        let Claim::Claimed(claimed_a) = Plan::VvecSync {
            q,
        }
        .claim(&quorum) else {
            panic!("expected Claimed for job A");
        };
        let mut job_a = claimed_a.start(&quorum, &empty_mns, ProTxHash::default()).unwrap();
        let done_a = job_a.poll_quorum(T0, true, &quorum, false, &[], &mut tracker);
        assert_eq!(done_a.next, Next::Done);
        assert!(!quorum.recovery_running());

        // Job B claims the same quorum; a stray extra `poll_quorum` on finished job A must NOT
        // clear job B's claim.
        let Claim::Claimed(claimed_b) = (Plan::SkShare {
            q,
            mask: CONTRIB,
            trigger,
        })
        .claim(&quorum) else {
            panic!("expected Claimed for job B");
        };
        assert!(quorum.recovery_running());

        let extra_a = job_a.poll_quorum(T0 + 1, true, &quorum, false, &[], &mut tracker);
        assert_eq!(
            extra_a,
            Poll {
                acts: Vec::new(),
                log: Vec::new(),
                next: Next::Done,
            }
        );
        assert!(quorum.recovery_running());
        assert_eq!(
            Plan::VvecSync {
                q,
            }
            .claim(&quorum),
            Claim::AlreadyRunning("NetQuorum::StartVvecSyncThread -- Already running")
        );
        claimed_b.release(&quorum);
        assert!(!quorum.recovery_running());

        // Same invariant when a job finishes via the sync-wait abort path.
        let Claim::Claimed(claimed_c) = Plan::VvecSync {
            q,
        }
        .claim(&quorum) else {
            panic!("expected Claimed for job C");
        };
        let mut job_c = claimed_c.start(&quorum, &empty_mns, ProTxHash::default()).unwrap();
        let abort_c = job_c.poll_quorum(T0, false, &quorum, true, &[], &mut tracker);
        assert_eq!(abort_c.next, Next::Done);
        assert!(!quorum.recovery_running());

        let Claim::Claimed(claimed_d) = Plan::VvecSync {
            q,
        }
        .claim(&quorum) else {
            panic!("expected Claimed for job D");
        };
        assert!(quorum.recovery_running());

        let extra_c = job_c.poll_quorum(T0 + 1, false, &quorum, true, &[], &mut tracker);
        assert_eq!(extra_c.next, Next::Done);
        assert!(quorum.recovery_running());
        claimed_d.release(&quorum);
        assert!(!quorum.recovery_running());
    }
}

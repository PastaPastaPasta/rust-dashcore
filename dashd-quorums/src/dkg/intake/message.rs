//! `NetDKG::ProcessMessage`, `AlreadyHave` and `ProcessGetData`.

use std::collections::{BTreeMap, HashMap};
use std::sync::MutexGuard;

use dash_consensus::primitives::HashWriter;
use dash_llmq::dkg::bls::BlsSchemeKind;
use dash_llmq::dkg::framing::{check_wire_structure, max_dkg_message_size};
use dash_llmq::dkg::session::{DkgInv, DkgMsgKind, Sender};
use dash_llmq::qdata::PeerId;
use dash_params::{LlmqParams, LlmqType};
use dash_primitives::QuorumHash;
use dash_types::Numeric;
use tracing::{debug, info};

use super::{DkgIntake, slot};
use crate::dkg::SessionKey;
use crate::dkg::log::uint256_hex;
use crate::dkg::seams::{GetDataResponse, IntakeHost};

/// The DKG messages `NetDKG` takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DkgNetMsg {
    /// `qwatch`.
    Qwatch,
    /// `qcontrib`, `qcomplaint`, `qjustify` or `qpcommit`.
    Dkg(DkgMsgKind),
}

/// What the host does with the peer after `ProcessMessage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing: the message was dropped, and any line was logged.
    Ignored,
    /// `pfrom.qwatch = true`.
    SetQwatch,
    /// `PeerMisbehaving(pfrom, score, message)`; an empty message is Core's default.
    Misbehaving {
        /// The score.
        score: i32,
        /// The reason.
        message: &'static str,
    },
    /// The payload went to its handler's queue, which may drop it as seen or over quota.
    Dispatched,
    /// A local read failed: the message is dropped unscored, and the host reports it as
    /// `ProcessMessages` reports a caught exception (DEC-1881).
    ReadFailed,
}

const fn misbehaving(score: i32, message: &'static str) -> Verdict {
    Verdict::Misbehaving {
        score,
        message,
    }
}

/// `UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE`.
// Core: v24.0.0 src/msg_result.h:21
const UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE: i32 = 10;

/// `NetMsgType` of `kind`.
// Core: v24.0.0 src/protocol.cpp:72-75
pub(crate) fn command(kind: DkgMsgKind) -> &'static str {
    match kind {
        DkgMsgKind::Contribution => "qcontrib",
        DkgMsgKind::Complaint => "qcomplaint",
        DkgMsgKind::Justification => "qjustify",
        DkgMsgKind::PrematureCommitment => "qpcommit",
        _ => "",
    }
}

/// The inventory hash of a raw payload: `CHashWriter(SER_GETHASH, 0)` over its bytes.
// Core: v24.0.0 src/llmq/net_dkg.cpp:629-632 NetDKG::ProcessMessage
pub(crate) fn payload_hash(payload: &[u8]) -> [u8; 32] {
    let mut w = HashWriter::new();
    w.input(payload);
    w.finish()
}

/// `unordered_lru_cache<uint256, int>`: past twice its size it keeps the `max` entries used
/// last.
// Core: v24.0.0 src/unordered_lru_cache.h:14-115 unordered_lru_cache
struct Lru {
    max: usize,
    map: HashMap<[u8; 32], (i32, u64)>,
    counter: u64,
}

impl Lru {
    fn get(&mut self, key: &[u8; 32]) -> Option<i32> {
        let (value, used) = self.map.get_mut(key)?;
        *used = self.counter;
        self.counter += 1;
        Some(*value)
    }

    fn insert(&mut self, key: [u8; 32], value: i32) {
        self.map.insert(key, (value, self.counter));
        self.counter += 1;
        if self.map.len() > self.max * 2 {
            let mut used: Vec<u64> = self.map.values().map(|(_, u)| *u).collect();
            used.sort_unstable_by(|a, b| b.cmp(a));
            let oldest_kept = used.get(self.max.saturating_sub(1)).copied().unwrap_or(u64::MAX);
            self.map.retain(|_, (_, u)| *u >= oldest_kept && self.max > 0);
        }
    }
}

/// `PerLlmqTypeCache<int>`: a quorum hash's quorum index, one LRU per configured type, sized
/// `keepOldConnections`; other types miss and drop writes.
// Core: v24.0.0 src/llmq/cache.h:25-102 PerLlmqTypeCache
pub(super) struct IndexCache(BTreeMap<u8, Lru>);

impl IndexCache {
    pub(super) fn new(llmqs: &BTreeMap<u8, LlmqParams>) -> Self {
        Self(
            llmqs
                .iter()
                .map(|(t, p)| {
                    let lru = Lru {
                        max: usize::from(p.keep_old_connections),
                        map: HashMap::new(),
                        counter: 0,
                    };
                    (*t, lru)
                })
                .collect(),
        )
    }

    fn get(&mut self, t: u8, hash: &QuorumHash) -> Option<i32> {
        self.0.get_mut(&t)?.get(hash.as_bytes())
    }

    fn insert(&mut self, t: u8, hash: &QuorumHash, index: i32) {
        if let Some(lru) = self.0.get_mut(&t) {
            lru.insert(*hash.as_bytes(), index);
        }
    }
}

impl DkgIntake {
    fn index_cache(&self) -> MutexGuard<'_, IndexCache> {
        self.indexed.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `NetDKG::ProcessMessage` of `msg` from `peer`, in Core's order: spork 17; `qwatch`; the
    /// observer drop; the MNAuth gate; the prefix; the type; the base block and the quorum
    /// index (cached per type); the size cap; the framing; the `getdata` we sent; the handler's
    /// queue.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:505-679 NetDKG::ProcessMessage
    pub fn process_message(&self, peer: PeerId, msg: DkgNetMsg, payload: Vec<u8>) -> Verdict {
        self.process_message_with(self.host.as_ref(), peer, msg, payload)
    }

    /// [`Self::process_message`] with `host` for this message's peer reads, the message
    /// thread's own context for the peer.
    pub fn process_message_with(
        &self,
        host: &dyn IntakeHost,
        peer: PeerId,
        msg: DkgNetMsg,
        payload: Vec<u8>,
    ) -> Verdict {
        if !self.seams.sporks.quorum_dkg_enabled() {
            return Verdict::Ignored;
        }
        let kind = match msg {
            DkgNetMsg::Qwatch if self.sessions.is_active() => return Verdict::SetQwatch,
            DkgNetMsg::Qwatch => return misbehaving(10, ""),
            DkgNetMsg::Dkg(kind) => kind,
        };
        let Some(slot) = slot(kind) else {
            return Verdict::Ignored;
        };
        if !self.sessions.is_active() {
            debug!(target: "llmq-dkg", "NetDKG -- ignoring {} in observer mode", command(kind));
            return Verdict::Ignored;
        }
        let Some(sender) = host.verified_pro_tx(peer).filter(|p| !p.is_null()) else {
            return misbehaving(10, "DKG message from non-verified peer");
        };
        let Some(prefix) = payload.get(1..33) else {
            return misbehaving(100, "malformed DKG message");
        };
        let t = payload[0];
        let quorum_hash = QuorumHash::from(<[u8; 32]>::try_from(prefix).expect("32 bytes"));
        let Some(params) = self.llmqs.get(&t) else {
            info!("NetDKG -- invalid llmqType [{t}]");
            return misbehaving(100, "");
        };
        let llmq_type = LlmqType::from_base(t);
        let cached = self.index_cache().get(t, &quorum_hash);
        let quorum_index = match cached {
            Some(index) => index,
            None => match self.quorum_index(host, llmq_type, params, &quorum_hash) {
                Ok(index) => index,
                Err(verdict) => return verdict,
            },
        };
        if payload.len() > max_dkg_message_size(kind, params) {
            return misbehaving(100, "oversized DKG message");
        }
        if !check_wire_structure(kind, &payload, params) {
            return misbehaving(100, "malformed DKG message");
        }
        let hash = payload_hash(&payload);
        let inv = DkgInv {
            kind,
            hash,
        };
        if host.consume_getdata_response(peer, inv) == GetDataResponse::Unrequested {
            debug!(
                target: "llmq-dkg",
                "NetDKG -- received unrequested {} {}, peer={peer}",
                command(kind),
                uint256_hex(&hash)
            );
            return misbehaving(UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE, "unrequested DKG message");
        }
        let key = SessionKey {
            llmq_type,
            quorum_index,
        };
        let Some(queues) = self.queues.get(&key) else {
            info!("NetDKG -- no session handlers for quorumIndex [{quorum_index}]");
            return misbehaving(100, "");
        };
        queues[slot].push(Sender::Peer(peer), &sender, payload, hash);
        self.index_cache().insert(t, &quorum_hash, quorum_index);
        Verdict::Dispatched
    }

    /// The quorum index of a base block the cache missed, or the verdict that drops it.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:571-600 NetDKG::ProcessMessage
    fn quorum_index(
        &self,
        host: &dyn IntakeHost,
        t: LlmqType,
        params: &LlmqParams,
        quorum_hash: &QuorumHash,
    ) -> Result<i32, Verdict> {
        let hash = uint256_hex(quorum_hash.as_bytes());
        let Some(base) = self.seams.chain.lookup(quorum_hash) else {
            info!("NetDKG -- unknown quorumHash {hash}");
            return Err(misbehaving(10, ""));
        };
        if base.height == 0 {
            info!("NetDKG -- parentless quorumHash {hash}");
            return Err(misbehaving(100, ""));
        }
        match host.quorum_type_enabled(t, &base) {
            Some(true) => {}
            Some(false) => {
                info!("NetDKG -- llmqType [{}] quorums aren't active", t.to_base());
                return Err(misbehaving(100, ""));
            }
            None => return Err(Verdict::ReadFailed),
        }
        let index = base.height % i32::from(params.dkg_interval);
        let max = if self.seams.chain.rotation_enabled(params, &base) {
            i32::from(params.signing_active_quorum_count) - 1
        } else {
            0
        };
        if index > max {
            info!("NetDKG -- invalid quorumHash {hash}");
            return Err(misbehaving(100, ""));
        }
        Ok(index)
    }

    /// `AlreadyHave(inv)` for a DKG inv: always for an observer, never with spork 17 off, else
    /// whether any handler's queue has seen its hash.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:681-704 NetDKG::AlreadyHave
    pub fn already_have(&self, inv: DkgInv) -> bool {
        if slot(inv.kind).is_none() {
            return false;
        }
        if !self.sessions.is_active() {
            return true;
        }
        if !self.seams.sporks.quorum_dkg_enabled() {
            return false;
        }
        self.queues.values().flatten().any(|q| q.has_seen(&inv.hash))
    }

    /// `ProcessGetData(inv)`: the message to send, of `inv`'s kind and serialized under the scheme
    /// `legacy` names (`bls::bls_legacy_scheme` at the call), or nothing for an observer or a miss.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:706-748 NetDKG::ProcessGetData
    pub fn process_getdata(&self, inv: DkgInv, legacy: bool) -> Option<(DkgMsgKind, Vec<u8>)> {
        if !self.sessions.is_active() {
            return None;
        }
        let flag = if legacy {
            BlsSchemeKind::Legacy
        } else {
            BlsSchemeKind::Basic
        };
        let s = &self.sessions;
        let payload = match inv.kind {
            DkgMsgKind::Contribution => s.get_contribution(&inv.hash)?.payload(flag),
            DkgMsgKind::Complaint => s.get_complaint(&inv.hash)?.payload(flag),
            DkgMsgKind::Justification => s.get_justification(&inv.hash)?.payload(flag),
            DkgMsgKind::PrematureCommitment => s.get_premature_commitment(&inv.hash)?.payload(flag),
            _ => return None,
        };
        Some((inv.kind, payload))
    }
}

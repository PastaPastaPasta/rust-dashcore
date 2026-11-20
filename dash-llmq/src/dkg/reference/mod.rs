//! Stand-ins for the node's side of a DKG session (feature `reference`): members with operator
//! keys, an in-memory dkgdb and debug status, and Core's relay topology. For tests, harnesses
//! and comparators only (LQ4 §3.1).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dash_consensus::primitives::{OutPoint, TxHash};
use dash_evo::llmq::{LlmqCommitment, MnEntryView, OperatorKeyView};
use dash_evo::types::ProTxHash;
use dash_types::Numeric;

use super::bls::{
    BlsIesMultiBlobs, BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue, IesSeed, decrypt_share,
    make_new_key, secret_to_public, sign_stand_in, stored_vvec_key,
};
use super::rng::{DkgRng, KeyDraw, RangeDraw, SeededRng};
use super::session::{
    DkgDebugSink, DkgStore, FinalVerifier, MemberStatusBit, MissingVvec, OperatorKey,
    SessionStatusBit, VerifiedContributions,
};
use super::sim::SimError;
use super::wire::{LlmqType, QuorumHash};

mod network;
mod topology;
mod verifier;

pub use network::{Network, NetworkSetup, Node, Queued, RngBuilder, node_seed, seeded_rng};
pub use topology::{CoreRelayTopology, quorum_relay_members};
pub use verifier::{ReferenceVerifier, VerifyCall};

/// One draw a [`ForcedRng`] answered, without its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawTag {
    /// A [`DkgRng::new_key`] draw.
    Key(KeyDraw),
    /// A [`DkgRng::ies_seed`] draw for this many recipients.
    Ies(usize),
    /// A [`DkgRng::rand_bool`] draw at this rate in percent.
    Bool(SimError, u8),
    /// A [`DkgRng::rand_range`] draw below `n`.
    Range(RangeDraw, u64),
}

/// The tags a [`ForcedRng`] recorded, shared with the handle [`ForcedRng::log`] gave out.
pub type DrawLog = Arc<Mutex<Vec<DrawTag>>>;

/// A [`DkgRng`] that delegates to `R` but answers the commit-lie type draw with a set value
/// (Core draws `GetRand<int>(5)` only after `COMMIT_LIE` fires, `a/dkgsession.cpp:584-588`), and
/// records the tag of every draw in order.
#[derive(Debug)]
pub struct ForcedRng<R> {
    inner: R,
    commit_lie_type: Option<u64>,
    log: DrawLog,
}

impl<R: DkgRng> ForcedRng<R> {
    /// A delegate to `inner` that forces nothing yet.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            commit_lie_type: None,
            log: DrawLog::default(),
        }
    }

    /// `rand_range(CommitLieType)` returns `lie_type` from now on.
    pub fn with_commit_lie_type(mut self, lie_type: u64) -> Self {
        self.commit_lie_type = Some(lie_type);
        self
    }

    /// A handle on the recorded tags, still valid once the RNG is boxed into a node.
    pub fn log(&self) -> DrawLog {
        self.log.clone()
    }

    fn record(&self, tag: DrawTag) {
        self.log.lock().expect("no panic holds the log").push(tag);
    }
}

impl<R: DkgRng> DkgRng for ForcedRng<R> {
    fn new_key(&mut self, why: KeyDraw) -> BlsSkValue {
        self.record(DrawTag::Key(why));
        self.inner.new_key(why)
    }

    fn ies_seed(&mut self, recipients: usize) -> IesSeed {
        self.record(DrawTag::Ies(recipients));
        self.inner.ies_seed(recipients)
    }

    fn rand_bool(&mut self, percent: u8, why: SimError) -> bool {
        self.record(DrawTag::Bool(why, percent));
        self.inner.rand_bool(percent, why)
    }

    fn rand_range(&mut self, n: u64, why: RangeDraw) -> u64 {
        self.record(DrawTag::Range(why, n));
        match (why, self.commit_lie_type) {
            (RangeDraw::CommitLieType, Some(forced)) => {
                assert!(forced < n, "the forced lie type is in range");
                forced
            }
            _ => self.inner.rand_range(n, why),
        }
    }
}

/// A masternode with an operator key, as the member list hands it to a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestMember {
    /// `proTxHash`.
    pub pro_tx_hash: ProTxHash,
    /// The collateral outpoint.
    pub collateral: OutPoint,
    /// `pubKeyOperator.Get()`.
    pub operator_pk: BlsPkValue,
}

impl TestMember {
    /// A member whose collateral is derived from its proTxHash.
    pub fn new(pro_tx_hash: ProTxHash, operator_pk: BlsPkValue) -> Self {
        Self {
            collateral: OutPoint::new(TxHash::from(*pro_tx_hash.as_bytes()), 0),
            pro_tx_hash,
            operator_pk,
        }
    }
}

impl MnEntryView for TestMember {
    fn pro_tx_hash(&self) -> &ProTxHash {
        &self.pro_tx_hash
    }
    fn collateral(&self) -> &OutPoint {
        &self.collateral
    }
    fn is_evo(&self) -> bool {
        false
    }
    fn is_banned(&self) -> bool {
        false
    }
    fn confirmed_hash_is_null(&self) -> bool {
        false
    }
    fn confirmed_hash_with_protx(&self) -> &[u8; 32] {
        self.pro_tx_hash.as_bytes()
    }
}

impl OperatorKeyView for TestMember {
    fn operator_key(&self) -> BlsPkValue {
        self.operator_pk.clone()
    }
}

/// `n` members with fresh operator keys from `seed`, and those keys: member `i`'s proTxHash
/// is the seed's eight bytes, then `i`, then zeros.
pub fn quorum(n: usize, seed: u64) -> (Vec<TestMember>, Vec<TestOperatorKey>) {
    let mut rng = SeededRng::new(seed);
    (0..n)
        .map(|i| {
            let key = TestOperatorKey::new(make_new_key(&mut rng));
            let mut pro_tx = [0u8; 32];
            pro_tx[..8].copy_from_slice(&seed.to_le_bytes());
            pro_tx[8..16].copy_from_slice(&(i as u64).to_le_bytes());
            (TestMember::new(ProTxHash::from(pro_tx), key.public_key()), key)
        })
        .unzip()
}

/// An operator secret key: AM1's `Sign` and `Decrypt`. It signs through the typed-API
/// stand-in, which signs as [`super::bls::sign`] does.
#[derive(Clone, Debug)]
pub struct TestOperatorKey {
    sk: BlsSkValue,
}

impl TestOperatorKey {
    /// The key `sk`.
    pub fn new(sk: BlsSkValue) -> Self {
        Self {
            sk,
        }
    }

    /// Its public key.
    pub fn public_key(&self) -> BlsPkValue {
        secret_to_public(&self.sk)
    }
}

impl OperatorKey for TestOperatorKey {
    fn public_key(&self) -> BlsPkValue {
        secret_to_public(&self.sk)
    }

    fn sign(&self, hash: &[u8; 32], legacy: bool) -> BlsSigValue {
        sign_stand_in(&self.sk, hash, legacy)
    }

    fn decrypt_share(
        &self,
        blobs: &BlsIesMultiBlobs,
        idx: usize,
        flag: BlsSchemeKind,
    ) -> Option<BlsSkValue> {
        decrypt_share(blobs, idx, &self.sk, flag)
    }
}

/// One dkgdb write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreRow {
    /// `qdkg_V`.
    Vvec {
        /// The contributor.
        who: ProTxHash,
        /// Its verification vector.
        vvec: Vec<BlsPkValue>,
    },
    /// `qdkg_S`.
    Sk {
        /// The contributor.
        who: ProTxHash,
        /// Our verified share from it.
        sk: BlsSkValue,
    },
    /// `qdkg_E`.
    Encrypted {
        /// The contributor.
        who: ProTxHash,
        /// Its encrypted contributions.
        blobs: BlsIesMultiBlobs,
    },
}

/// One access [`MemStore::verified_contributions`] made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreRead {
    /// `GetAllQuorumMembers`.
    Members,
    /// A `qdkg_V` read of the member's row.
    Vvec(ProTxHash),
    /// A `qdkg_S` read of the member's row.
    Sk(ProTxHash),
}

/// A `contributionsCache` entry: the vector and the share the first read found.
type CacheEntry = (Vec<BlsPkValue>, BlsSkValue);

/// An in-memory dkgdb that keeps every write in order, with the type and base it was for, and
/// every access its reads make.
#[derive(Clone, Debug, Default)]
pub struct MemStore {
    /// `(type, base, row)` per write. A member's rows once read are answered from the cache, as
    /// Core's are, so a later write or edit of them is not seen.
    pub rows: Vec<(LlmqType, QuorumHash, StoreRow)>,
    /// What `GetAllQuorumMembers` gives at every base: the session's members, in Core's order.
    pub members: Vec<ProTxHash>,
    /// Every access of [`DkgStore::verified_contributions`], in order.
    pub reads: Vec<StoreRead>,
    /// `contributionsCache`, which never expires here.
    cache: BTreeMap<(u8, QuorumHash, ProTxHash), CacheEntry>,
}

impl MemStore {
    /// A store whose members are `members`.
    pub fn with_members(members: Vec<ProTxHash>) -> Self {
        Self {
            members,
            ..Self::default()
        }
    }

    /// The number of members accesses made.
    pub fn member_accesses(&self) -> usize {
        self.reads.iter().filter(|read| **read == StoreRead::Members).count()
    }

    /// The last row written for `(t, base)` that `pick` takes.
    fn last_row<T>(
        &self,
        t: LlmqType,
        base: &QuorumHash,
        pick: impl Fn(&StoreRow) -> Option<T>,
    ) -> Option<T> {
        self.rows
            .iter()
            .rev()
            .filter(|(row_t, row_base, _)| *row_t == t && row_base == base)
            .find_map(|(_, _, row)| pick(row))
    }

    /// The rows of one kind, as `who` in write order: `"V"`, `"S"` or `"E"`.
    pub fn writers(&self, kind: &str) -> Vec<ProTxHash> {
        self.rows
            .iter()
            .filter_map(|(_, _, row)| match (kind, row) {
                (
                    "V",
                    StoreRow::Vvec {
                        who,
                        ..
                    },
                )
                | (
                    "S",
                    StoreRow::Sk {
                        who,
                        ..
                    },
                )
                | (
                    "E",
                    StoreRow::Encrypted {
                        who,
                        ..
                    },
                ) => Some(*who),
                _ => None,
            })
            .collect()
    }
}

impl DkgStore for MemStore {
    fn write_vvec(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, vvec: &[BlsPkValue]) {
        let row = StoreRow::Vvec {
            who: *who,
            vvec: vvec.to_vec(),
        };
        self.rows.push((t, *base, row));
    }

    fn write_sk(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, sk: &BlsSkValue) {
        let row = StoreRow::Sk {
            who: *who,
            sk: sk.clone(),
        };
        self.rows.push((t, *base, row));
    }

    fn write_encrypted(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        who: &ProTxHash,
        blobs: &BlsIesMultiBlobs,
    ) {
        let row = StoreRow::Encrypted {
            who: *who,
            blobs: blobs.clone(),
        };
        self.rows.push((t, *base, row));
    }

    fn verified_contributions(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        valid: &[bool],
    ) -> Result<VerifiedContributions, MissingVvec> {
        assert!(!self.members.is_empty(), "the store's members are set");
        self.reads.push(StoreRead::Members);
        let mut out = VerifiedContributions::default();
        for i in 0..self.members.len() {
            if !valid.get(i).copied().unwrap_or(false) {
                continue;
            }
            let who = self.members[i];
            let key = (t.to_base(), *base, who);
            if !self.cache.contains_key(&key) {
                self.reads.push(StoreRead::Vvec(who));
                let vvec = self.last_row(t, base, |row| match row {
                    StoreRow::Vvec {
                        who: w,
                        vvec,
                    } if *w == who => Some(vvec.clone()),
                    _ => None,
                });
                let Some(vvec) = vvec else {
                    return Err(MissingVvec {
                        who,
                    });
                };
                let vvec = vvec.iter().map(stored_vvec_key).collect();
                self.reads.push(StoreRead::Sk(who));
                let sk = self.last_row(t, base, |row| match row {
                    StoreRow::Sk {
                        who: w,
                        sk,
                    } if *w == who => Some(sk.clone()),
                    _ => None,
                });
                self.cache.insert(key, (vvec, sk.unwrap_or_else(BlsSkValue::invalid)));
            }
            let (vvec, sk) = &self.cache[&key];
            out.member_indexes.push(i as u16);
            out.vvecs.push(vvec.clone());
            out.sk_contributions.push(sk.clone());
        }
        Ok(out)
    }
}

/// A [`FinalVerifier`] with a fixed verdict that keeps every commitment it was asked about.
#[derive(Clone, Debug, Default)]
pub struct RecordingVerifier {
    /// The verdict: `false` by default.
    pub accept: bool,
    /// `(base, commitment)` per call.
    pub calls: Vec<(QuorumHash, LlmqCommitment)>,
}

impl FinalVerifier for RecordingVerifier {
    fn verify(&mut self, base: &QuorumHash, qc: &LlmqCommitment) -> bool {
        self.calls.push((*base, qc.clone()));
        self.accept
    }
}

/// One debug status update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugUpdate {
    /// `InitLocalSessionStatus`.
    InitSession {
        /// The quorum's base hash.
        base: QuorumHash,
        /// Its height.
        height: i32,
    },
    /// `UpdateLocalSessionStatus`.
    Session(SessionStatusBit),
    /// `UpdateLocalMemberStatus`.
    Member(usize, MemberStatusBit),
    /// `UpdateLocalMemberStatus` of the first member, adding the second to its
    /// `complaintsFromMembers`.
    ComplaintFrom(usize, usize),
}

/// A debug sink that keeps every update in order.
#[derive(Clone, Debug, Default)]
pub struct RecordingDebug {
    /// The updates.
    pub updates: Vec<DebugUpdate>,
}

impl DkgDebugSink for RecordingDebug {
    fn init_local_session_status(
        &mut self,
        _t: LlmqType,
        _quorum_index: i32,
        base: &QuorumHash,
        base_height: i32,
    ) {
        self.updates.push(DebugUpdate::InitSession {
            base: *base,
            height: base_height,
        });
    }

    fn update_local_session_status(
        &mut self,
        _t: LlmqType,
        _quorum_index: i32,
        bit: SessionStatusBit,
    ) {
        self.updates.push(DebugUpdate::Session(bit));
    }

    fn update_local_member_status(
        &mut self,
        _t: LlmqType,
        _quorum_index: i32,
        idx: usize,
        bit: MemberStatusBit,
    ) {
        self.updates.push(DebugUpdate::Member(idx, bit));
    }

    fn add_local_member_complaint(
        &mut self,
        _t: LlmqType,
        _quorum_index: i32,
        idx: usize,
        from: usize,
    ) {
        self.updates.push(DebugUpdate::ComplaintFrom(idx, from));
    }
}

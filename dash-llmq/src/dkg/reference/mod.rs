//! Stand-ins for the node's side of a DKG session (feature `reference`): members with operator
//! keys, an in-memory dkgdb and debug status, and Core's relay topology. For tests, harnesses
//! and comparators only (LQ4 §3.1).

use dash_consensus::primitives::{OutPoint, TxHash};
use dash_evo::llmq::{MnEntryView, OperatorKeyView};
use dash_evo::types::ProTxHash;

use super::bls::{
    BlsIesMultiBlobs, BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue, decrypt_share,
    make_new_key, secret_to_public, sign_stand_in,
};
use super::rng::SeededRng;
use super::session::{DkgDebugSink, DkgStore, MemberStatusBit, OperatorKey, SessionStatusBit};
use super::wire::{LlmqType, QuorumHash};

mod network;
mod topology;

pub use network::{Network, Node, Queued};
pub use topology::{CoreRelayTopology, quorum_relay_members};

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
            collateral: OutPoint {
                hash: TxHash::from(*pro_tx_hash.as_bytes()),
                index: 0,
            },
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
/// stand-in until BS11c's `core::sign` exists (P-1).
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

/// An in-memory dkgdb that keeps every write in order, with the type and base it was for.
#[derive(Clone, Debug, Default)]
pub struct MemStore {
    /// `(type, base, row)` per write.
    pub rows: Vec<(LlmqType, QuorumHash, StoreRow)>,
}

impl MemStore {
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
}

//! Recording fakes of the [`super::source`] seams, shared by the `qdata` tests.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use dash_consensus::primitives::BlockHash;
use dash_evo::types::ProTxHash;
use dash_params::LlmqType;
use dash_types::Numeric;

use super::source::{
    EncryptedContributions, OperatorDecrypt, QuorumData, QuorumDataSource, ValidMnsAt,
};
use crate::dkg::bls::{BlsPkValue, BlsSchemeKind, BlsSkValue};
use crate::dkg::wire::BlsIesBlobBytes;

/// A quorum's state. `set_vvec` and `set_sk_share` record every call and store their argument
/// only when the matching `accept_*` flag is set.
#[derive(Debug, Default)]
pub struct FakeQuorumState {
    pub threshold: usize,
    pub members: Vec<ProTxHash>,
    pub valid: Vec<bool>,
    pub vvec: RefCell<Option<Arc<Vec<BlsPkValue>>>>,
    pub sk_share: RefCell<Option<BlsSkValue>>,
    pub accept_vvec: bool,
    pub accept_sk_share: bool,
    pub set_vvec_calls: RefCell<Vec<(Vec<BlsPkValue>, BlsSchemeKind)>>,
    pub set_sk_share_calls: RefCell<Vec<(BlsSkValue, ProTxHash)>>,
    pub recovery_claimed: Cell<bool>,
}

/// A shared handle to a [`FakeQuorumState`]: clones see the same state.
#[derive(Clone, Debug)]
pub struct FakeQuorum(pub Rc<FakeQuorumState>);

impl From<FakeQuorumState> for FakeQuorum {
    fn from(state: FakeQuorumState) -> Self {
        Self(Rc::new(state))
    }
}

impl FakeQuorum {
    pub fn state(&self) -> &FakeQuorumState {
        &self.0
    }
}

impl QuorumData for FakeQuorum {
    fn threshold(&self) -> usize {
        self.0.threshold
    }

    fn valid_count(&self) -> usize {
        self.0.valid.iter().filter(|v| **v).count()
    }

    fn size(&self) -> usize {
        self.0.valid.len()
    }

    fn member_index(&self, p: &ProTxHash) -> Option<usize> {
        self.0.members.iter().position(|m| m == p)
    }

    fn is_valid_member(&self, p: &ProTxHash) -> bool {
        self.member_index(p).is_some_and(|i| self.0.valid.get(i) == Some(&true))
    }

    fn members(&self) -> &[ProTxHash] {
        &self.0.members
    }

    fn valid(&self) -> &[bool] {
        &self.0.valid
    }

    fn vvec(&self) -> Option<Arc<Vec<BlsPkValue>>> {
        self.0.vvec.borrow().clone()
    }

    fn has_sk_share(&self) -> bool {
        self.0.sk_share.borrow().is_some()
    }

    fn set_vvec(&self, v: Vec<BlsPkValue>, scheme: BlsSchemeKind) -> bool {
        self.0.set_vvec_calls.borrow_mut().push((v.clone(), scheme));
        if self.0.accept_vvec {
            *self.0.vvec.borrow_mut() = Some(Arc::new(v));
        }
        self.0.accept_vvec
    }

    fn set_sk_share(&self, sk: BlsSkValue, me: &ProTxHash) -> bool {
        self.0.set_sk_share_calls.borrow_mut().push((sk.clone(), *me));
        if self.0.accept_sk_share {
            *self.0.sk_share.borrow_mut() = Some(sk);
        }
        self.0.accept_sk_share
    }

    fn try_claim_recovery(&self) -> bool {
        !self.0.recovery_claimed.replace(true)
    }

    fn release_recovery(&self) {
        self.0.recovery_claimed.set(false);
    }

    fn recovery_running(&self) -> bool {
        self.0.recovery_claimed.get()
    }
}

/// [`FakeSource`]'s quorum read error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FakeReadError;

/// Quorums by `(type, hash)`, with every `quorum` lookup recorded. A lookup of a key in
/// `read_errors` fails.
#[derive(Debug, Default)]
pub struct FakeSource {
    pub registered: BTreeSet<u8>,
    pub read_errors: BTreeSet<(u8, BlockHash)>,
    pub active: BTreeSet<BlockHash>,
    pub quorums: BTreeMap<(u8, BlockHash), FakeQuorum>,
    pub cached: BTreeMap<(u8, BlockHash), FakeQuorum>,
    pub quorum_lookups: RefCell<Vec<(u8, BlockHash)>>,
}

impl QuorumDataSource for FakeSource {
    type Q = FakeQuorum;
    type Error = FakeReadError;

    fn llmq_registered(&self, t: LlmqType) -> bool {
        self.registered.contains(&t.to_base())
    }

    fn active_block(&self, h: &BlockHash) -> bool {
        self.active.contains(h)
    }

    fn quorum(&self, t: LlmqType, h: &BlockHash) -> Result<Option<FakeQuorum>, FakeReadError> {
        self.quorum_lookups.borrow_mut().push((t.to_base(), *h));
        if self.read_errors.contains(&(t.to_base(), *h)) {
            return Err(FakeReadError);
        }
        Ok(self.quorums.get(&(t.to_base(), *h)).cloned())
    }

    fn cached_quorum(&self, t: LlmqType, h: &BlockHash) -> Option<FakeQuorum> {
        self.cached.get(&(t.to_base(), *h)).cloned()
    }
}

/// One [`EncryptedContributions::get`] call: the type, base block, valid bits and member.
pub type ContributionsCall = (u8, BlockHash, Vec<bool>, ProTxHash);

/// dkgdb rows by `(type, base, member)`, with every read recorded.
#[derive(Debug, Default)]
pub struct FakeContributions {
    pub rows: BTreeMap<(u8, BlockHash, ProTxHash), Vec<BlsIesBlobBytes>>,
    pub calls: RefCell<Vec<ContributionsCall>>,
}

impl EncryptedContributions for FakeContributions {
    fn get(
        &self,
        t: LlmqType,
        base: &BlockHash,
        valid: &[bool],
        p: &ProTxHash,
    ) -> Option<Vec<BlsIesBlobBytes>> {
        self.calls.borrow_mut().push((t.to_base(), *base, valid.to_vec(), *p));
        self.rows.get(&(t.to_base(), *base, *p)).cloned()
    }
}

/// An operator key whose decryptions are looked up by the blob's ciphertext and the recipient
/// index, with every call recorded.
#[derive(Debug, Default)]
pub struct FakeOperator {
    pub pro_tx: Option<ProTxHash>,
    pub shares: BTreeMap<(Vec<u8>, usize), BlsSkValue>,
    pub calls: RefCell<Vec<(BlsIesBlobBytes, usize)>>,
}

impl OperatorDecrypt for FakeOperator {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.pro_tx
    }

    fn decrypt_sk(&self, b: &BlsIesBlobBytes, idx: usize) -> Option<BlsSkValue> {
        self.calls.borrow_mut().push((b.clone(), idx));
        self.shares.get(&(b.data().to_vec(), idx)).cloned()
    }
}

/// Enabled masternode lists by block.
#[derive(Debug, Default)]
pub struct FakeValidMns {
    pub lists: BTreeMap<BlockHash, Vec<ProTxHash>>,
}

impl ValidMnsAt for FakeValidMns {
    fn valid_pro_tx_hashes(&self, block: &BlockHash) -> Option<Vec<ProTxHash>> {
        self.lists.get(block).cloned()
    }
}

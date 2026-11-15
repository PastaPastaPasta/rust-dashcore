//! The session's `CBLSWorkerCache` (v24.0.0 `bls/bls_worker.h:145-198`), one per session as
//! Core's is (`l/dkgsession.h:130`).

use std::collections::BTreeMap;

use crate::dkg::bls::{BlsPkValue, BlsSkValue};

/// What `SendCommitment` and `ReceiveMessage(CDKGPrematureCommitment)` build from the verified
/// contributions, keyed by the contributions' member indexes (`memberIndexes`). Core keys on
/// their `SerializeHash`, which groups the index lists the same way.
///
/// Every value built is kept, failures included. A key holds neither the scheme nor the
/// inputs, so a later call with the same key gets the first value, whatever the scheme or the
/// store's rows are then. The quorum verification vector is kept, never its hash: each receive
/// hashes it under its own scheme (`l/dkgsession.cpp:606-607`).
#[derive(Clone, Debug, Default)]
pub(super) struct SessionCache {
    /// `vvecCache`: `BuildQuorumVerificationVector`'s vector, `None` where it built none.
    quorum_vvec: BTreeMap<Vec<u16>, Option<Vec<BlsPkValue>>>,
    /// `secretKeyShareCache`: `AggregateSecretKeys`' sum, the invalid key included.
    share_sum: BTreeMap<Vec<u16>, BlsSkValue>,
    /// `publicKeyShareCache`: `BuildPubKeyShare`'s share, keyed with the sender's id too.
    pk_share: BTreeMap<(Vec<u16>, [u8; 32]), BlsPkValue>,
    /// Every call builds and nothing is kept.
    #[cfg(any(test, feature = "reference"))]
    off: bool,
}

impl SessionCache {
    /// A cache that keeps nothing: each call builds.
    #[cfg(any(test, feature = "reference"))]
    pub(super) fn off() -> Self {
        Self {
            off: true,
            ..Self::default()
        }
    }

    /// `BuildQuorumVerificationVector(SerializeHash(memberIndexes), vvecs)`
    /// (`a/dkgsession.cpp:563`; `l/dkgsession.cpp:589`).
    pub(super) fn quorum_vvec(
        &mut self,
        member_indexes: &[u16],
        build: impl FnOnce() -> Option<Vec<BlsPkValue>>,
    ) -> Option<Vec<BlsPkValue>> {
        get_or_build(self.is_off(), &mut self.quorum_vvec, member_indexes.to_vec(), build)
    }

    /// `AggregateSecretKeys(SerializeHash(memberIndexes), skContributions)`
    /// (`a/dkgsession.cpp:571`).
    pub(super) fn share_sum(
        &mut self,
        member_indexes: &[u16],
        build: impl FnOnce() -> BlsSkValue,
    ) -> BlsSkValue {
        get_or_build(self.is_off(), &mut self.share_sum, member_indexes.to_vec(), build)
    }

    /// `BuildPubKeyShare(SerializeHash(make_pair(memberIndexes, member->id)), vvec, id)`
    /// (`l/dkgsession.cpp:612`).
    pub(super) fn pk_share(
        &mut self,
        member_indexes: &[u16],
        id: &[u8; 32],
        build: impl FnOnce() -> BlsPkValue,
    ) -> BlsPkValue {
        get_or_build(self.is_off(), &mut self.pk_share, (member_indexes.to_vec(), *id), build)
    }

    #[cfg(any(test, feature = "reference"))]
    fn is_off(&self) -> bool {
        self.off
    }

    #[cfg(not(any(test, feature = "reference")))]
    fn is_off(&self) -> bool {
        false
    }

    /// The values kept in `vvecCache`, `secretKeyShareCache` and `publicKeyShareCache`: each
    /// was built once, by the first call with its key.
    #[cfg(test)]
    pub(super) fn entries(&self) -> [usize; 3] {
        [self.quorum_vvec.len(), self.share_sum.len(), self.pk_share.len()]
    }
}

/// `GetOrBuild` (`bls/bls_worker.h:179-197`): the value kept for `key`, or else `build`'s,
/// which is kept unless the cache is `off`.
fn get_or_build<K: Ord, V: Clone>(
    off: bool,
    map: &mut BTreeMap<K, V>,
    key: K,
    build: impl FnOnce() -> V,
) -> V {
    if off {
        return build();
    }
    map.entry(key).or_insert_with(build).clone()
}

#[cfg(test)]
mod tests;

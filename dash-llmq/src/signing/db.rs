//! Core's recovered-signature and signing-vote database.

use dash_consensus::primitives::CoreWrite;
use dash_evo::locks::wire::RecoveredSig;
use dash_params::LlmqType;
use dash_types::Numeric;

use super::keys;

/// Core's `CDBBatch` size at which a cleanup pass writes its batch and starts a new one.
// Core: v24.0.0 src/llmq/signing.cpp:258, :303
const BATCH_FLUSH_BYTES: usize = 1 << 24;

/// One operation in an atomic recovered-signature DB update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecSigOp {
    /// Stores the value under the raw key, replacing any previous value.
    Put(Vec<u8>, Vec<u8>),
    /// Removes the raw key; removing an absent key is not an error.
    Delete(Vec<u8>),
}

/// An ordered list of puts and deletes, applied atomically by [`RecSigKv::write`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecSigBatch {
    pub ops: Vec<RecSigOp>,
    size_estimate: usize,
}

impl RecSigBatch {
    // Core: v24.0.0 src/dbwrapper.h:101-127 CDBBatch::Write
    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.size_estimate += 3
            + usize::from(key.len() > 127)
            + key.len()
            + usize::from(value.len() > 127)
            + value.len();
        self.ops.push(RecSigOp::Put(key, value));
    }

    // Core: v24.0.0 src/dbwrapper.h:129-147 CDBBatch::Erase
    pub fn delete(&mut self, key: Vec<u8>) {
        self.size_estimate += 2 + usize::from(key.len() > 127) + key.len();
        self.ops.push(RecSigOp::Delete(key));
    }

    // Core: v24.0.0 src/dbwrapper.h:149 CDBBatch::SizeEstimate
    fn size_estimate(&self) -> usize {
        self.size_estimate
    }
}

/// A fallible cursor over raw, deobfuscated Core key/value bytes.
pub type RecSigScan<'a, E> = Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>), E>> + 'a>;

/// Ordered byte storage used by Core's recsig database.
pub trait RecSigKv {
    type Error;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;
    /// Rows are in ascending byte-key order, beginning at `start` (inclusive), and all come
    /// from one consistent snapshot taken when the cursor opens: rows written after that are
    /// not returned. Callers must drop the iterator before writing. Both opening and advancing
    /// the cursor can fail.
    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, Self::Error>, Self::Error>;
    /// Applies all operations in order, atomically; an error leaves the store unchanged.
    fn write(&mut self, batch: RecSigBatch) -> Result<(), Self::Error>;
}

/// Core's `CRecoveredSigsDb`, without its LRU caches. Time and Platform policy are injected.
pub struct RecSigDb<K> {
    kv: K,
    platform_type: LlmqType,
}

impl<K> RecSigDb<K> {
    /// Uses the chain's configured Platform quorum type for both reads and writes. Pass the
    /// effective value after any devnet `-llmqplatform` override, not the network default.
    // Core: v24.0.0 src/llmq/signing.cpp:24-27 CRecoveredSigsDb
    pub fn new(kv: K, platform_type: LlmqType) -> Self {
        Self {
            kv,
            platform_type,
        }
    }

    /// Returns the underlying store.
    pub fn into_inner(self) -> K {
        self.kv
    }

    /// Borrows the underlying store.
    pub fn store(&self) -> &K {
        &self.kv
    }

    /// Mutably borrows the underlying store.
    pub fn store_mut(&mut self) -> &mut K {
        &mut self.kv
    }

    /// Core compares the numeric enum values, so a non-canonical `Unknown(n)` still matches.
    fn is_platform(&self, t: LlmqType) -> bool {
        t.to_base() == self.platform_type.to_base()
    }
}

impl<K: RecSigKv> RecSigDb<K> {
    /// Platform compares `(type, id, msg)`; other quorum types compare `(type, id)`.
    // Core: v24.0.0 src/llmq/signing.cpp:664-668 CSigningManager::HasRecoveredSigForSigning
    pub fn has_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, K::Error> {
        if self.is_platform(t) {
            self.has_recovered_sig(t, id, msg)
        } else {
            self.has_for_id(t, id)
        }
    }

    // Core: v24.0.0 src/llmq/signing.cpp:31-39 HasRecoveredSig
    pub fn has_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, K::Error> {
        if self.is_platform(t)
            && self.kv.get(&keys::recovered_platform_message(t, id, msg))?.is_some()
        {
            return Ok(true);
        }
        Ok(self.kv.get(&keys::recovered_message(t, id, msg))?.is_some())
    }

    // Core: v24.0.0 src/llmq/signing.cpp:41-59 HasRecoveredSigForId
    pub fn has_for_id(&self, t: LlmqType, id: &[u8; 32]) -> Result<bool, K::Error> {
        Ok(self.kv.get(&keys::recovered(t, id))?.is_some())
    }

    // Core: v24.0.0 src/llmq/signing.cpp:61-77 HasRecoveredSigForSession
    pub fn has_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, K::Error> {
        Ok(self.kv.get(&keys::recovered_session(sign_hash))?.is_some())
    }

    // Core: v24.0.0 src/llmq/signing.cpp:79-95 HasRecoveredSigForHash
    pub fn has_for_hash(&self, hash: &[u8; 32]) -> Result<bool, K::Error> {
        Ok(self.kv.get(&keys::recovered_hash(hash))?.is_some())
    }

    // Core: v24.0.0 src/llmq/signing.cpp:97-112 ReadRecoveredSig
    fn read_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, K::Error> {
        Ok(self.kv.get(&keys::recovered(t, id))?.and_then(|value| decode_sig(&value)))
    }

    // Core: v24.0.0 src/llmq/signing.cpp:114-126 GetRecoveredSigByHash
    pub fn get_by_hash(&self, hash: &[u8; 32]) -> Result<Option<RecoveredSig>, K::Error> {
        if let Some(sig) =
            self.kv.get(&keys::recovered_platform(hash))?.and_then(|v| decode_sig(&v))
        {
            return Ok(Some(sig));
        }
        let Some((t, id)) =
            self.kv.get(&keys::recovered_hash(hash))?.and_then(|v| keys::decode_type_id(&v))
        else {
            return Ok(None);
        };
        self.read_recovered_sig(t, &id)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:128-131 GetRecoveredSigById
    pub fn get_by_id(&self, t: LlmqType, id: &[u8; 32]) -> Result<Option<RecoveredSig>, K::Error> {
        self.read_recovered_sig(t, id)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:133-144 GetRecoveredSig
    pub fn get(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, K::Error> {
        if self.is_platform(t)
            && let Some(hash) = self
                .kv
                .get(&keys::recovered_platform_message(t, id, msg))?
                .and_then(|value| value.get(..32).and_then(|v| v.try_into().ok()))
        {
            return self.get_by_hash(&hash);
        }
        Ok(self.read_recovered_sig(t, id)?.filter(|sig| sig.msg_hash == *msg))
    }

    // Core: v24.0.0 src/llmq/signing.cpp:146-186 WriteRecoveredSig
    pub fn write(&mut self, sig: &RecoveredSig, now_s: i64) -> Result<(), K::Error> {
        let hash = sig.hash();
        let now = now_s as u32;
        let mut batch = RecSigBatch::default();
        if self.is_platform(sig.llmq_type) {
            batch.put(keys::recovered_platform(&hash), encode_sig(sig));
            batch.put(
                keys::recovered_platform_message(sig.llmq_type, &sig.id, &sig.msg_hash),
                hash.to_vec(),
            );
            batch.put(keys::recovered_platform_time(now, &hash), vec![1]);
        } else {
            batch.put(keys::recovered(sig.llmq_type, &sig.id), encode_sig(sig));
            batch.put(
                keys::recovered_message(sig.llmq_type, &sig.id, &sig.msg_hash),
                now.to_le_bytes().to_vec(),
            );
            batch.put(keys::recovered_time(now, sig.llmq_type, &sig.id), vec![1]);
        }
        let mut type_id = Vec::with_capacity(33);
        type_id.write(&[sig.llmq_type.to_base()]);
        type_id.write(&sig.id);
        batch.put(keys::recovered_hash(&hash), type_id);
        batch.put(keys::recovered_session(&sig.sign_hash()), vec![1]);
        self.kv.write(batch)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:188-224 RemoveRecoveredSig. Both v24 callers pass
    // `deleteTimeKey = false`, so that branch is not ported.
    fn remove_recovered_sig(
        &self,
        batch: &mut RecSigBatch,
        t: LlmqType,
        id: &[u8; 32],
        delete_hash_key: bool,
    ) -> Result<(), K::Error> {
        let Some(sig) = self.read_recovered_sig(t, id)? else {
            return Ok(());
        };
        batch.delete(keys::recovered(sig.llmq_type, &sig.id));
        batch.delete(keys::recovered_message(sig.llmq_type, &sig.id, &sig.msg_hash));
        if delete_hash_key {
            batch.delete(keys::recovered_hash(&sig.hash()));
            batch.delete(keys::recovered_session(&sig.sign_hash()));
        }
        Ok(())
    }

    /// Removes by-id records, retaining hash/session indexes for late-share filtering.
    /// Fresh Platform records are unaffected, as they have no by-id record.
    // Core: v24.0.0 src/llmq/signing.cpp:226-234 TruncateRecoveredSig
    pub fn truncate(&mut self, t: LlmqType, id: &[u8; 32]) -> Result<(), K::Error> {
        let mut batch = RecSigBatch::default();
        self.remove_recovered_sig(&mut batch, t, id, false)?;
        self.kv.write(batch)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:236-264 CleanupOldPlatformSigs
    fn cleanup_old_platform(&mut self, max_age: i64, now_s: i64) -> Result<(), K::Error> {
        let end_time = now_s.wrapping_sub(max_age) as u32;
        let mut entries = Vec::new();
        {
            let cursor = self.kv.scan_from(&keys::recovered_platform_time(0, &[0; 32]))?;
            for row in cursor {
                let (key, _) = row?;
                let Some((time, hash)) = keys::decode_platform_time(&key) else {
                    break;
                };
                if time >= end_time {
                    break;
                }
                entries.push((time, hash));
            }
        }
        let mut batch = RecSigBatch::default();
        for (time, hash) in entries {
            if let Some(sig) =
                self.kv.get(&keys::recovered_platform(&hash))?.and_then(|v| decode_sig(&v))
            {
                batch.delete(keys::recovered_platform(&hash));
                batch.delete(keys::recovered_platform_message(
                    sig.llmq_type,
                    &sig.id,
                    &sig.msg_hash,
                ));
                batch.delete(keys::recovered_hash(&hash));
                batch.delete(keys::recovered_session(&sig.sign_hash()));
            }
            // Core erases the decoded key re-serialized, not the stored bytes.
            batch.delete(keys::recovered_platform_time(time, &hash));
            if batch.size_estimate() >= BATCH_FLUSH_BYTES {
                self.kv.write(std::mem::take(&mut batch))?;
            }
        }
        self.kv.write(batch)
    }

    /// Removes time indexes strictly older than `now_s - max_age`, Platform first.
    /// Each batch is atomic; an error can follow earlier successful batches.
    // Core: v24.0.0 src/llmq/signing.cpp:266-316 CleanupOldRecoveredSigs
    pub fn cleanup_old(&mut self, max_age: i64, now_s: i64) -> Result<(), K::Error> {
        self.cleanup_old_platform(max_age, now_s)?;
        let end_time = now_s.wrapping_sub(max_age) as u32;
        let mut entries = Vec::new();
        {
            let cursor =
                self.kv.scan_from(&keys::recovered_time(0, LlmqType::from_base(0), &[0; 32]))?;
            for row in cursor {
                let (key, _) = row?;
                let Some((time, t, id)) = keys::decode_typed_time(&key, b"rs_t") else {
                    break;
                };
                if time >= end_time {
                    break;
                }
                entries.push((time, t, id));
            }
        }
        if entries.is_empty() {
            return Ok(());
        }
        let mut batch = RecSigBatch::default();
        for (_, t, id) in &entries {
            self.remove_recovered_sig(&mut batch, *t, id, true)?;
            if batch.size_estimate() >= BATCH_FLUSH_BYTES {
                self.kv.write(std::mem::take(&mut batch))?;
            }
        }
        for (time, t, id) in entries {
            batch.delete(keys::recovered_time(time, t, &id));
        }
        self.kv.write(batch)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:318-322 HasVotedOnId
    pub fn has_voted(&self, t: LlmqType, id: &[u8; 32]) -> Result<bool, K::Error> {
        Ok(self.kv.get(&keys::vote(t, id))?.is_some())
    }

    // Core: v24.0.0 src/llmq/signing.cpp:324-328 GetVoteForId
    pub fn vote_for(&self, t: LlmqType, id: &[u8; 32]) -> Result<Option<[u8; 32]>, K::Error> {
        Ok(self
            .kv
            .get(&keys::vote(t, id))?
            .and_then(|v| v.get(..32).and_then(|b| b.try_into().ok())))
    }

    // Core: v24.0.0 src/llmq/signing.cpp:330-340 WriteVoteForId
    pub fn write_vote(
        &mut self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
        now_s: i64,
    ) -> Result<(), K::Error> {
        let mut batch = RecSigBatch::default();
        batch.put(keys::vote(t, id), msg.to_vec());
        batch.put(keys::vote_time(now_s as u32, t, id), vec![1]);
        self.kv.write(batch)
    }

    // Core: v24.0.0 src/llmq/signing.cpp:342-381 CleanupOldVotes
    pub fn cleanup_old_votes(&mut self, max_age: i64, now_s: i64) -> Result<(), K::Error> {
        let end_time = now_s.wrapping_sub(max_age) as u32;
        let mut batch = RecSigBatch::default();
        {
            let cursor =
                self.kv.scan_from(&keys::vote_time(0, LlmqType::from_base(0), &[0; 32]))?;
            for row in cursor {
                let (key, _) = row?;
                let Some((time, t, id)) = keys::decode_typed_time(&key, b"rs_vt") else {
                    break;
                };
                if time >= end_time {
                    break;
                }
                batch.delete(keys::vote_time(time, t, &id));
                batch.delete(keys::vote(t, &id));
            }
        }
        if batch.ops.is_empty() {
            return Ok(());
        }
        self.kv.write(batch)
    }
}

// Core: v24.0.0 src/llmq/signing.h:96-101 CRecoveredSig::SerializationOp
fn encode_sig(sig: &RecoveredSig) -> Vec<u8> {
    let mut out = Vec::with_capacity(RecoveredSig::LEN);
    out.write(&[sig.llmq_type.to_base()]);
    out.write(sig.quorum_hash.as_bytes());
    out.write(&sig.id);
    out.write(&sig.msg_hash);
    out.write(&sig.sig);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:97-112 ReadRecoveredSig
fn decode_sig(value: &[u8]) -> Option<RecoveredSig> {
    RecoveredSig::decode(&mut &value[..])
}

#[cfg(test)]
mod tests;

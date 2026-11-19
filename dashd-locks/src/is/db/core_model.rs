//! S1a's model of Core's InstantSend store, shared by the store's, the listener's and the node's
//! tests (PLAN-LK1b §2.3, §2.4, §3.1): every answer, row and cache transition is Core's.

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::Tx;
use dash_consensus::connect::has_lock_inputs;
use dash_consensus::primitives::{OutPoint, TxHash};
use dash_evo::locks::isdlock_hash;
use dash_types::codec::BaseCodec;

use super::{CacheSnapshot, ChainedRemoval};
use crate::is::{IsLockHash, LockedTx, lock_inputs};

/// The store's rows, by key.
pub type Rows = BTreeMap<Vec<u8>, Vec<u8>>;
/// One batch: puts (`Some`) and deletes (`None`) in order.
pub type Changes = Vec<(Vec<u8>, Option<Vec<u8>>)>;

/// Core's `unordered_lru_cache` as an ordered list, least recent first: 20,000 entries, then
/// truncated to the 10,000 most recent.
pub struct Cache<K, V>(pub Vec<(K, V)>);

impl<K, V> Default for Cache<K, V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<K: Eq, V: Clone> Cache<K, V> {
    pub fn get(&mut self, key: &K) -> Option<V> {
        let index = self.0.iter().position(|(k, _)| k == key)?;
        let entry = self.0.remove(index);
        let value = entry.1.clone();
        self.0.push(entry);
        Some(value)
    }

    pub fn put(&mut self, key: K, value: V) {
        self.erase(&key);
        self.0.push((key, value));
        if self.0.len() > 20_000 {
            self.0.drain(..self.0.len() - 10_000);
        }
    }

    pub fn erase(&mut self, key: &K) {
        self.0.retain(|(k, _)| k != key);
    }

    pub fn erase_if_maps_to(&mut self, key: &K, value: &V)
    where
        V: PartialEq,
    {
        self.0.retain(|(k, v)| k != key || v != value);
    }
}

pub fn prefix(name: &str) -> Vec<u8> {
    let mut bytes = vec![name.len() as u8];
    bytes.extend_from_slice(name.as_bytes());
    bytes
}

pub fn key(name: &str, suffix: &[u8]) -> Vec<u8> {
    let mut bytes = prefix(name);
    bytes.extend_from_slice(suffix);
    bytes
}

pub fn outpoint_key(outpoint: &OutPoint) -> Vec<u8> {
    let mut bytes = key("is_in", outpoint.txid().as_bytes());
    bytes.extend_from_slice(&outpoint.index().to_le_bytes());
    bytes
}

pub fn height_key(name: &str, height: i32, hash: &IsLockHash) -> Vec<u8> {
    let mut bytes = prefix(name);
    bytes.extend_from_slice(&(!(height as u32)).to_be_bytes());
    bytes.extend_from_slice(hash);
    bytes
}

pub fn apply(rows: &mut Rows, changes: &Changes) {
    for (key, value) in changes {
        if let Some(value) = value {
            rows.insert(key.clone(), value.clone());
        } else {
            rows.remove(key);
        }
    }
}

/// Core v24.0.0's `CInstantSendDb` (`instantsend/db.cpp`) as plain data: its rows, its three caches,
/// `best_confirmed_height`, and every batch it committed, with DEC-1631's cache rule for a root
/// that is not stored.
pub struct Model {
    pub rows: Rows,
    pub hashes: Cache<IsLockHash, Option<LockedTx>>,
    pub txids: Cache<TxHash, IsLockHash>,
    pub inputs: Cache<OutPoint, IsLockHash>,
    pub confirmed: i32,
    pub journal: Vec<Changes>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            rows: BTreeMap::from([(prefix("is_v"), vec![1, 0, 0, 0])]),
            hashes: Cache::default(),
            txids: Cache::default(),
            inputs: Cache::default(),
            confirmed: 0,
            journal: Vec::new(),
        }
    }
}

impl Model {
    /// The model of a store reopened on `rows`: empty caches and `best_confirmed_height = 0`
    /// (PLAN-LK1b §2.4 oracle step 2).
    pub fn reopened(rows: Rows) -> Self {
        Self {
            rows,
            ..Self::default()
        }
    }

    pub fn commit(&mut self, changes: Changes) {
        apply(&mut self.rows, &changes);
        self.journal.push(changes);
    }

    /// Core's `WriteInstantSendLockMined`: one batch with the mined mark.
    pub fn write_mined(&mut self, hash: IsLockHash, height: i32) {
        self.commit(vec![(height_key("is_m", height, &hash), Some(vec![1]))]);
    }

    pub fn reopen(&mut self) {
        self.hashes = Cache::default();
        self.txids = Cache::default();
        self.inputs = Cache::default();
        self.confirmed = 0;
    }

    pub fn by_hash(&mut self, hash: &IsLockHash, cached: bool) -> Option<LockedTx> {
        if *hash == [0; 32] {
            return None;
        }
        if cached && let Some(value) = self.hashes.get(hash) {
            return value;
        }
        let value = self
            .rows
            .get(&key("is_i", hash))
            .and_then(|v| LockedTx::decode(&mut v.as_slice()).ok())
            .filter(|lock| isdlock_hash(lock) == *hash);
        self.hashes.put(*hash, value.clone());
        value
    }

    pub fn hash_by_txid(&mut self, txid: &TxHash) -> IsLockHash {
        if let Some(hash) = self.txids.get(txid) {
            return hash;
        }
        let Some(value) = self.rows.get(&key("is_tx", txid.as_bytes())) else {
            return [0; 32];
        };
        let hash = value[..32].try_into().unwrap();
        self.txids.put(*txid, hash);
        hash
    }

    pub fn by_txid(&mut self, txid: &TxHash) -> Option<LockedTx> {
        let hash = self.hash_by_txid(txid);
        self.by_hash(&hash, true)
    }

    pub fn by_input(&mut self, input: &OutPoint) -> Option<LockedTx> {
        let hash = if let Some(hash) = self.inputs.get(input) {
            hash
        } else {
            let value = self.rows.get(&outpoint_key(input))?;
            let hash = value[..32].try_into().unwrap();
            self.inputs.put(*input, hash);
            hash
        };
        self.by_hash(&hash, true)
    }

    pub fn known(&mut self, hash: &IsLockHash) -> bool {
        self.by_hash(hash, true).is_some() || self.rows.contains_key(&key("is_a2", hash))
    }

    pub fn write(&mut self, hash: IsLockHash, lock: &LockedTx) {
        let mut value = Vec::new();
        lock.encode(&mut value);
        let mut changes = vec![
            (key("is_i", &hash), Some(value)),
            (key("is_tx", lock.txid().as_bytes()), Some(hash.to_vec())),
        ];
        for outpoint in lock.inputs() {
            changes.push((outpoint_key(outpoint), Some(hash.to_vec())));
        }
        self.commit(changes);
        self.hashes.put(hash, Some(lock.clone()));
        self.txids.put(lock.txid(), hash);
        for outpoint in lock.inputs() {
            self.inputs.put(*outpoint, hash);
        }
    }

    fn remove(&mut self, changes: &mut Changes, hash: IsLockHash, lock: &LockedTx, keep: bool) {
        changes.push((key("is_i", &hash), None));
        changes.push((key("is_tx", lock.txid().as_bytes()), None));
        for outpoint in lock.inputs() {
            changes.push((outpoint_key(outpoint), None));
        }
        if !keep {
            self.hashes.erase(&hash);
            self.txids.erase(&lock.txid());
            for outpoint in lock.inputs() {
                self.inputs.erase(outpoint);
            }
        }
    }

    fn erase_root_entries(&mut self, hash: IsLockHash, root: &LockedTx) {
        self.hashes.erase(&hash);
        self.txids.erase_if_maps_to(&root.txid(), &hash);
        for outpoint in root.inputs() {
            self.inputs.erase_if_maps_to(outpoint, &hash);
        }
    }

    fn archive(changes: &mut Changes, hash: IsLockHash, height: i32) {
        changes.push((height_key("is_a1", height, &hash), Some(vec![1])));
        changes.push((key("is_a2", &hash), Some(vec![1])));
    }

    fn heights(&self, name: &str, height: i32) -> Vec<(Vec<u8>, i32, IsLockHash)> {
        let start = height_key(name, height, &[0; 32]);
        self.rows
            .range(start..)
            .take_while(|(k, _)| k.starts_with(&prefix(name)))
            .map(|(k, _)| {
                let offset = name.len() + 1;
                let h = !u32::from_be_bytes(k[offset..offset + 4].try_into().unwrap());
                (k.clone(), h as i32, k[offset + 4..offset + 36].try_into().unwrap())
            })
            .collect()
    }

    pub fn confirm(&mut self, height: i32) -> BTreeMap<IsLockHash, LockedTx> {
        if height <= self.confirmed {
            return BTreeMap::new();
        }
        self.confirmed = height;
        let mut changes = Vec::new();
        let mut result = BTreeMap::new();
        for (key, h, hash) in self.heights("is_m", height) {
            if let Some(lock) = self.by_hash(&hash, false) {
                self.remove(&mut changes, hash, &lock, true);
                result.entry(hash).or_insert(lock);
            }
            Self::archive(&mut changes, hash, h);
            changes.push((key, None));
        }
        self.commit(changes);
        result
    }

    pub fn prune(&mut self, height: i32) {
        if height <= 0 {
            return;
        }
        let mut changes = Vec::new();
        for (k, _, hash) in self.heights("is_a1", height) {
            changes.push((key("is_a2", &hash), None));
            changes.push((k, None));
        }
        self.commit(changes);
    }

    pub fn children(&self, parent: &TxHash) -> Vec<IsLockHash> {
        let start = key("is_in", parent.as_bytes());
        self.rows
            .range(start.clone()..)
            .take_while(|(k, _)| k.starts_with(&start))
            .map(|(_, value)| value[..32].try_into().unwrap())
            .collect()
    }

    pub fn chained(&mut self, hash: IsLockHash, root: &LockedTx, height: i32) -> ChainedRemoval {
        let mut stack = vec![root.txid()];
        let mut seen = BTreeSet::new();
        let mut changes = Vec::new();
        let mut hashes = Vec::new();
        while let Some(parent) = stack.pop() {
            for child in self.children(&parent) {
                if let Some(lock) = self.by_hash(&child, false) {
                    self.remove(&mut changes, child, &lock, false);
                    Self::archive(&mut changes, child, height);
                    hashes.push(child);
                    if seen.insert(lock.txid()) {
                        stack.push(lock.txid());
                    }
                }
            }
        }
        let stored = self.by_hash(&hash, false);
        match &stored {
            Some(lock) => self.remove(&mut changes, hash, lock, false),
            // DEC-1631: the root lock's own cached entries are cleared when it is not stored too.
            None => self.erase_root_entries(hash, root),
        }
        Self::archive(&mut changes, hash, height);
        hashes.push(hash);
        self.commit(changes);
        ChainedRemoval {
            hashes,
            root_present: stored.is_some(),
        }
    }

    pub fn conflicting_lock(&mut self, tx: &Tx) -> Option<(IsLockHash, LockedTx)> {
        for input in lock_inputs(tx) {
            let Some(lock) = self.by_input(&input) else {
                continue;
            };
            if lock.txid() != tx.txid() {
                return Some((isdlock_hash(&lock), lock));
            }
        }
        None
    }

    pub fn snapshot(&self) -> CacheSnapshot {
        CacheSnapshot {
            locks: self.hashes.0.clone(),
            txids: self.txids.0.clone(),
            inputs: self.inputs.0.clone(),
        }
    }

    pub fn block(&mut self, transactions: &[Tx], height: i32, connected: bool) {
        let mut changes = Vec::new();
        for tx in transactions {
            if !has_lock_inputs(tx) {
                continue;
            }
            let hash = self.hash_by_txid(&tx.txid());
            if hash != [0; 32] {
                changes.push((height_key("is_m", height, &hash), connected.then(|| vec![1])));
            }
        }
        self.commit(changes);
    }
}

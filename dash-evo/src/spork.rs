//! Sporks: Core's `CSporkMessage` hashes and signatures, `CSporkManager`'s validation, store and
//! value rules (`spork.{h,cpp}`, `messagesigner.cpp`, `CPubKey::RecoverCompact`), the shared spork
//! core both products use (SB4 D29). The node's runtime (intake, `getsporks`, persistence) is
//! `dashd-masternode::sporks` (SP0).
//!
//! v24 only (DEC-82): five sporks, one signer key, no `-minsporkkeys`. Every function takes the
//! time as a value; the caller supplies Core's `GetAdjustedTime()`.

use std::collections::BTreeMap;

use dash_consensus::hash::{hash160, sha256d};
use dash_p2p_core::Spork;
use dash_params::{ChainParams, NetworkKind};
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::{Message, PublicKey, SecretKey};

/// A spork's `GetHash()`: SHA256d of its Core serialization, signature included.
pub type SporkHash = [u8; 32];

/// A key ID (`CKeyID`): HASH160 of a serialized public key.
pub type KeyId = [u8; 20];

/// Core's `SporkId`. A peer may send any value; only [`SPORK_DEFS`] have a name and a default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SporkId(pub i32);

impl SporkId {
    pub const SPORK_2_INSTANTSEND_ENABLED: SporkId = SporkId(10001);
    pub const SPORK_17_QUORUM_DKG_ENABLED: SporkId = SporkId(10016);
    pub const SPORK_19_CHAINLOCKS_ENABLED: SporkId = SporkId(10018);
    pub const SPORK_21_QUORUM_ALL_CONNECTED: SporkId = SporkId(10020);
    pub const SPORK_23_QUORUM_POSE: SporkId = SporkId(10022);
}

/// Core's `CSporkDef`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SporkDef {
    pub id: SporkId,
    pub default: i64,
    pub name: &'static str,
}

/// The default of every v24 spork: 2099-01-01, off.
pub const SPORK_OFF: i64 = 4_070_908_800;

/// Core's `sporkDefs`, in Core's order.
// Core: spork.h:66-72
pub const SPORK_DEFS: [SporkDef; 5] = [
    SporkDef {
        id: SporkId::SPORK_2_INSTANTSEND_ENABLED,
        default: SPORK_OFF,
        name: "SPORK_2_INSTANTSEND_ENABLED",
    },
    SporkDef {
        id: SporkId::SPORK_17_QUORUM_DKG_ENABLED,
        default: SPORK_OFF,
        name: "SPORK_17_QUORUM_DKG_ENABLED",
    },
    SporkDef {
        id: SporkId::SPORK_19_CHAINLOCKS_ENABLED,
        default: SPORK_OFF,
        name: "SPORK_19_CHAINLOCKS_ENABLED",
    },
    SporkDef {
        id: SporkId::SPORK_21_QUORUM_ALL_CONNECTED,
        default: SPORK_OFF,
        name: "SPORK_21_QUORUM_ALL_CONNECTED",
    },
    SporkDef {
        id: SporkId::SPORK_23_QUORUM_POSE,
        default: SPORK_OFF,
        name: "SPORK_23_QUORUM_POSE",
    },
];

/// The definition of `id`, if it is one of Core's.
pub fn def_of(id: SporkId) -> Option<&'static SporkDef> {
    SPORK_DEFS.iter().find(|d| d.id == id)
}

/// Core's `GetSporkIDByName`; `None` is Core's `SPORK_INVALID`.
// Core: spork.cpp:198-207
pub fn id_by_name(name: &str) -> Option<SporkId> {
    SPORK_DEFS.iter().find(|d| d.name == name).map(|d| d.id)
}

/// How far ahead of the adjusted time a spork may be signed.
// Core: spork.cpp:88
pub const MAX_TIME_AHEAD: i64 = 2 * 60 * 60;

/// Core's `MESSAGE_MAGIC`.
// Core: util/message.cpp:25
pub const MESSAGE_MAGIC: &str = "DarkCoin Signed Message:\n";

/// Core's `CPubKey::COMPACT_SIGNATURE_SIZE`.
pub const COMPACT_SIGNATURE_SIZE: usize = 65;

fn push_compact_size(out: &mut Vec<u8>, n: usize) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&(n as u64).to_le_bytes());
        }
    }
}

/// The spork as Core serializes it: `nSporkID ‖ nValue ‖ nTimeSigned ‖ vchSig`.
// Core: spork.h:113-117
pub fn core_bytes(spork: &Spork) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + 1 + spork.sig.len());
    out.extend_from_slice(&spork.spork_id.to_le_bytes());
    out.extend_from_slice(&spork.value.to_le_bytes());
    out.extend_from_slice(&spork.time_signed.to_le_bytes());
    push_compact_size(&mut out, spork.sig.len());
    out.extend_from_slice(&spork.sig);
    out
}

/// Core's `CSporkMessage::GetHash`: the hash relayed in `inv` and looked up by `getdata`.
// Core: spork.cpp:264-267
pub fn spork_hash(spork: &Spork) -> SporkHash {
    sha256d(&core_bytes(spork))
}

/// Core's `CSporkMessage::GetSignatureHash`: the testnet signing hash.
// Core: spork.cpp:269-276
pub fn signature_hash(spork: &Spork) -> [u8; 32] {
    let mut buf = [0u8; 20];
    buf[..4].copy_from_slice(&spork.spork_id.to_le_bytes());
    buf[4..12].copy_from_slice(&spork.value.to_le_bytes());
    buf[12..].copy_from_slice(&spork.time_signed.to_le_bytes());
    sha256d(&buf)
}

/// The string Core signs off testnet: `ToString(nSporkID) + ToString(nValue) +
/// ToString(nTimeSigned)`, each in decimal.
// Core: spork.cpp:301, 328
pub fn signed_message(spork: &Spork) -> String {
    format!("{}{}{}", spork.spork_id, spork.value, spork.time_signed)
}

/// Core's `CMessageSigner` hash: SHA256d of `MESSAGE_MAGIC ‖ message`, each with its
/// compact-size length.
// Core: messagesigner.cpp:38-44
pub fn message_hash(message: &str) -> [u8; 32] {
    let mut buf = Vec::with_capacity(2 + MESSAGE_MAGIC.len() + 9 + message.len());
    push_compact_size(&mut buf, MESSAGE_MAGIC.len());
    buf.extend_from_slice(MESSAGE_MAGIC.as_bytes());
    push_compact_size(&mut buf, message.len());
    buf.extend_from_slice(message.as_bytes());
    sha256d(&buf)
}

/// Which hash a network's spork key signs. Core picks it by `NetworkIDString() == "test"`
/// ("Harden Spork6"), so devnets and regtest sign the message like mainnet.
// Core: spork.cpp:287-312, 319-334
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigForm {
    /// Testnet: a compact signature over [`signature_hash`] (`CHashSigner`).
    Hash,
    /// Every other network: a signed message over [`signed_message`] (`CMessageSigner`).
    Message,
}

impl SigForm {
    pub fn of(params: &ChainParams) -> SigForm {
        match params.network {
            NetworkKind::Testnet => SigForm::Hash,
            _ => SigForm::Message,
        }
    }

    /// The hash the spork key signs for `spork`.
    pub fn signing_hash(self, spork: &Spork) -> [u8; 32] {
        match self {
            SigForm::Hash => signature_hash(spork),
            SigForm::Message => message_hash(&signed_message(spork)),
        }
    }
}

/// Core's `CPubKey::RecoverCompact` followed by `GetID()`.
///
/// Like Core, the header byte is not range-checked: the recovery id is `(header - 27) & 3` and
/// the compression flag `(header - 27) & 4`, so headers that differ in higher bits are aliases,
/// and a high-S signature recovers (only `VerifyHashCanonical` refuses both, and sporks do not
/// use it).
// Core: pubkey.cpp:199-217; messagesigner.cpp:91-105 CHashSigner::VerifyHash
pub fn recover_key_id(hash: &[u8; 32], sig: &[u8]) -> Option<KeyId> {
    if sig.len() != COMPACT_SIGNATURE_SIZE {
        return None;
    }
    let header = i32::from(sig[0]) - 27;
    let recid = RecoveryId::from_u8_masked((header & 3) as u8);
    let compressed = header & 4 != 0;
    let rsig = RecoverableSignature::from_compact(&sig[1..], recid).ok()?;
    let key = rsig.recover(Message::from_digest(*hash)).ok()?;
    Some(key_id_of(&key, compressed))
}

/// `CPubKey::GetID()`: HASH160 of the key as `compressed` serializes it.
pub fn key_id_of(key: &PublicKey, compressed: bool) -> KeyId {
    if compressed {
        hash160(&key.serialize())
    } else {
        hash160(&key.serialize_uncompressed())
    }
}

/// Core's `CSporkMessage::CheckSignature`.
// Core: spork.cpp:317-337
pub fn check_signature(spork: &Spork, key_id: &KeyId, form: SigForm) -> bool {
    recover_key_id(&form.signing_hash(spork), &spork.sig).as_ref() == Some(key_id)
}

/// Core's `CKey::SignCompact`: RFC 6979 with no extra entropy, so the signature is the one
/// Core's key would make.
// Core: key.cpp:254-275
pub fn sign_compact(hash: &[u8; 32], key: &SecretKey, compressed: bool) -> Vec<u8> {
    let (recid, rs) =
        RecoverableSignature::sign_ecdsa_recoverable(Message::from_digest(*hash), key)
            .serialize_compact();
    let mut sig = Vec::with_capacity(COMPACT_SIGNATURE_SIZE);
    sig.push(
        27 + recid.to_u8()
            + if compressed {
                4
            } else {
                0
            },
    );
    sig.extend_from_slice(&rs);
    sig
}

/// Core's `CSporkMessage(id, value, time)` followed by `Sign(key)`.
// Core: spork.cpp:278-315
pub fn sign(
    id: SporkId,
    value: i64,
    time_signed: i64,
    key: &SecretKey,
    compressed: bool,
    form: SigForm,
) -> Spork {
    let mut spork = Spork {
        spork_id: id.0,
        value,
        time_signed,
        sig: Vec::new(),
    };
    spork.sig = sign_compact(&form.signing_hash(&spork), key, compressed);
    spork
}

/// Why `IsValidSpork` refuses a spork (both score +100).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidSpork {
    /// `"ERROR: too far into the future"`.
    TooFarInFuture,
    /// `"ERROR: invalid signature"`, also when no spork key is set.
    InvalidSignature,
}

/// Core's `CSporkManager::IsValidSpork` with `adjusted_now` = `GetAdjustedTime()` in seconds.
// Core: spork.cpp:86-102
pub fn validate(
    spork: &Spork,
    adjusted_now: i64,
    key_id: Option<&KeyId>,
    form: SigForm,
) -> Result<(), InvalidSpork> {
    if spork.time_signed > adjusted_now.saturating_add(MAX_TIME_AHEAD) {
        return Err(InvalidSpork::TooFarInFuture);
    }
    match key_id {
        Some(key_id) if check_signature(spork, key_id, form) => Ok(()),
        _ => Err(InvalidSpork::InvalidSignature),
    }
}

/// Core's `IsSporkActive` for a value from [`SporkStore::value`]: active once the value, read as
/// a time, is before the adjusted time.
// Core: spork.cpp:163-169
pub fn is_active(value: i64, adjusted_now: i64) -> bool {
    value < adjusted_now
}

/// Core's hardened mainnet value: every spork 0 (active) except SPORK_21, which is 1 (O17).
// Core: spork.cpp:173-181
pub fn hardened_value(id: SporkId) -> i64 {
    if id == SporkId::SPORK_21_QUORUM_ALL_CONNECTED {
        1
    } else {
        0
    }
}

/// What `ProcessSpork` did with a valid spork.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Processed {
    /// The stored spork for its id is as new or newer: `"seen"`, nothing changes or relays.
    Seen,
    /// The first spork for its id: `"new"`, relayed.
    New,
    /// Newer than the stored one: `"updated"`, relayed.
    Updated,
}

impl Processed {
    /// Whether Core relays the spork (`ProcessSpork` returned true).
    pub fn relays(self) -> bool {
        self != Processed::Seen
    }
}

/// The bucket counts libstdc++'s `unordered_map` passes through as it grows from default
/// construction (`_Prime_rehash_policy`, max load factor 1): 13 at the first insert, then the
/// prime from its list at or above twice the count. Measured with g++ 15; the policy has not
/// changed since GCC 5, so Core's release builds share it.
const LIBSTDCXX_GROWTH: [usize; 20] = [
    1, 13, 29, 59, 127, 257, 541, 1109, 2357, 5087, 10273, 20753, 42043, 85229, 172933, 351061,
    712697, 1447153, 2938679, 5967347,
];

/// Core's `std::unordered_map<SporkId, CSporkMessage> mapSporksActive` in libstdc++'s iteration
/// order, which `getsporks` replies follow (`ActiveSporks`). `std::hash<SporkId>` is the
/// identity (`spork.h:47-56`), the bucket `id % bucket_count` after sign extension, and every
/// bucket's nodes are contiguous in the list: an insert goes before the first node of its bucket,
/// or to the front when the bucket is empty, and a rehash reinserts the list in order.
#[derive(Clone, Debug)]
struct ActiveSporks {
    list: Vec<Spork>,
    /// `bucket_count()`.
    buckets: usize,
    /// `_M_next_resize`: the size above which the next insert rehashes.
    next_resize: usize,
}

impl Default for ActiveSporks {
    fn default() -> Self {
        Self {
            list: Vec::new(),
            buckets: 1,
            next_resize: 0,
        }
    }
}

impl ActiveSporks {
    fn bucket(id: i32, buckets: usize) -> u64 {
        (i64::from(id) as u64) % buckets as u64
    }

    fn get(&self, id: i32) -> Option<&Spork> {
        self.list.iter().find(|s| s.spork_id == id)
    }

    fn link(list: &mut Vec<Spork>, spork: Spork, buckets: usize) {
        let b = Self::bucket(spork.spork_id, buckets);
        let at = list.iter().position(|s| Self::bucket(s.spork_id, buckets) == b).unwrap_or(0);
        list.insert(at, spork);
    }

    /// `map[id] = spork`.
    fn insert_or_assign(&mut self, spork: Spork) {
        match self.list.iter_mut().find(|s| s.spork_id == spork.spork_id) {
            Some(slot) => *slot = spork,
            None => self.insert_new(spork),
        }
    }

    /// `map.insert(spork)`: a spork whose id is present is dropped.
    fn insert(&mut self, spork: Spork) {
        if self.get(spork.spork_id).is_none() {
            self.insert_new(spork);
        }
    }

    // libstdc++: _M_insert_unique_node, _M_need_rehash, _M_insert_bucket_begin, _M_rehash_aux
    fn insert_new(&mut self, spork: Spork) {
        if self.list.len() + 1 > self.next_resize {
            // With load factor 1 the policy always grows here: to 13 (the minimum 11, plus one,
            // rounded up to a prime) from the default single bucket, else to the next count.
            let i = LIBSTDCXX_GROWTH.iter().position(|&n| n == self.buckets).unwrap_or(0);
            match LIBSTDCXX_GROWTH.get(i + 1) {
                Some(&n) => {
                    let old = std::mem::take(&mut self.list);
                    for s in old {
                        Self::link(&mut self.list, s, n);
                    }
                    self.buckets = n;
                    self.next_resize = n;
                }
                // Millions of distinctly signed spork ids: past the table, stop growing.
                None => self.next_resize = usize::MAX,
            }
        }
        Self::link(&mut self.list, spork, self.buckets);
    }
}

/// Core's `SporkStore`: every valid spork by hash (`mapSporksByHash`, for `getdata`) and the
/// newest per id (`mapSporksActive`, for values and `getsporks`).
#[derive(Clone, Debug, Default)]
pub struct SporkStore {
    by_hash: BTreeMap<SporkHash, Spork>,
    active: ActiveSporks,
}

impl SporkStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Core's `ProcessSpork` for a spork `IsValidSpork` accepted.
    // Core: spork.cpp:104-125
    pub fn process(&mut self, spork: Spork) -> Processed {
        let outcome = match self.active.get(spork.spork_id) {
            Some(old) if old.time_signed >= spork.time_signed => return Processed::Seen,
            Some(_) => Processed::Updated,
            None => Processed::New,
        };
        self.insert(spork);
        outcome
    }

    /// The store step of Core's `UpdateSpork`: `spork` replaces its id's entry whatever its time.
    // Core: spork.cpp:156-157
    pub fn insert(&mut self, spork: Spork) {
        self.by_hash.insert(spork_hash(&spork), spork.clone());
        self.active.insert_or_assign(spork);
    }

    /// Core's `ActiveSporks`, in Core's iteration order.
    // Core: spork.cpp:127-136
    pub fn active(&self) -> &[Spork] {
        &self.active.list
    }

    /// Core's `GetSporkByHash`.
    // Core: spork.cpp:209-217
    pub fn by_hash(&self, hash: &SporkHash) -> Option<&Spork> {
        self.by_hash.get(hash)
    }

    /// Every spork by hash, in hash order.
    pub fn all(&self) -> impl Iterator<Item = &Spork> {
        self.by_hash.values()
    }

    /// Core's `SporkValueIfActive`.
    // Core: spork.cpp:22-30
    pub fn active_value(&self, id: SporkId) -> Option<i64> {
        self.active.get(id.0).map(|s| s.value)
    }

    /// Core's `GetSporkValue`: the hardened value on mainnet (`hardened`), else the active
    /// spork's value, the default, or -1 for an id Core does not define.
    // Core: spork.cpp:171-196
    pub fn value(&self, id: SporkId, hardened: bool) -> i64 {
        if hardened {
            return hardened_value(id);
        }
        self.active_value(id).or_else(|| def_of(id).map(|d| d.default)).unwrap_or(-1)
    }

    /// Core's `CheckAndRemove`: drops every stored spork the current key did not sign. Nothing
    /// happens without a key.
    // Core: spork.cpp:62-84
    pub fn check_and_remove(&mut self, key_id: Option<&KeyId>, form: SigForm) {
        let Some(key_id) = key_id else {
            return;
        };
        self.active.list.retain(|s| check_signature(s, key_id, form));
        self.by_hash.retain(|_, s| check_signature(s, key_id, form));
    }

    /// Core's `Clear`. The bucket count stays, as `unordered_map::clear` keeps it.
    // Core: spork.cpp:32-39
    pub fn clear(&mut self) {
        self.active.list.clear();
        self.by_hash.clear();
    }

    /// Rebuilds a store from its two maps as saved: Core's `Unserialize` into a fresh store,
    /// which inserts `active` in the order it was written (the iteration order it had); of two
    /// sporks with one id the first stays.
    // Core: spork.h:154-169; serialize.h:1264-1267 UnserializeMap
    pub fn restore(by_hash: Vec<Spork>, active: Vec<Spork>) -> SporkStore {
        let mut store = SporkStore::new();
        for spork in by_hash {
            store.by_hash.insert(spork_hash(&spork), spork);
        }
        for spork in active {
            store.active.insert(spork);
        }
        store
    }
}

#[cfg(test)]
mod tests;

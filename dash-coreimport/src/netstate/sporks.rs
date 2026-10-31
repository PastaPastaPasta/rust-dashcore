//! `sporks.dat`: the spork messages the node has seen (`SporkStore`, `spork.h`), in the
//! `CFlatDB` envelope with the magic message `magicSporkCache`.
//!
//! The object is the version string `CSporkManager-Version-3`, then `mapSporksByHash`
//! (`uint256` to message) and `mapSporksActive` (spork id to message). A different version string
//! is not an error: Core leaves both maps empty and ignores the rest ([`SporksDat::version_matched`]).
//! A repeated key keeps its first entry (`unordered_map::insert`). Core's own maps are unordered
//! and salted per process, so the order here (sorted by key) is not the file's.
//!
//! Core runs `CheckAndRemove` after loading: every message whose signature does not verify
//! against the network's spork key is dropped. That needs the key and the network's signing mode
//! (testnet signs the hash, the other networks sign a message); see
//! [`SporkMessage::check_signature`]. **An importer must apply it**: this reader does not.

use std::collections::BTreeMap;

use bitcoin_hashes::{Hash, hash160, sha256d};
use dash_network::Network;
use secp256k1::Message;
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};

use super::error::FormatResult;
use super::flat::{self, Loaded, Payload};
use super::reader::Reader;

const WHAT: &str = "sporks.dat";

/// A few sporks, 4 KiB; this is far above anything Core writes.
pub const MAX_SPORKS_DAT_SIZE: usize = 16 << 20;

/// `SporkStore::SERIALIZATION_VERSION_STRING`.
pub const SERIALIZATION_VERSION_STRING: &str = "CSporkManager-Version-3";

/// `CPubKey::COMPACT_SIGNATURE_SIZE`: the most a spork signature may hold (`LIMITED_VECTOR`).
pub const MAX_SIGNATURE_SIZE: u64 = 65;

/// One `CSporkMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SporkMessage {
    /// The `SporkId` (`int32_t`); any value is kept, as Core's unchecked enum cast does.
    pub id: i32,
    pub value: i64,
    pub time_signed: i64,
    /// At most [`MAX_SIGNATURE_SIZE`] bytes.
    pub signature: Vec<u8>,
}

impl SporkMessage {
    // Core: spork.h CSporkMessage::SERIALIZE_METHODS
    fn read(r: &mut Reader<'_>) -> FormatResult<SporkMessage> {
        let id = r.i32_le()?;
        let value = r.i64_le()?;
        let time_signed = r.i64_le()?;
        let len = r.compact_size(true)?;
        if len > MAX_SIGNATURE_SIZE {
            return Err(r.corrupt("spork signature longer than 65 bytes"));
        }
        let signature = r.take(len as usize)?.to_vec();
        Ok(SporkMessage {
            id,
            value,
            time_signed,
            signature,
        })
    }

    /// `CSporkMessage::GetSignatureHash`: the double SHA-256 of the id, value and time alone.
    // Core: spork.cpp CSporkMessage::GetSignatureHash
    pub fn signature_hash(&self) -> [u8; 32] {
        let mut bytes = Vec::with_capacity(20);
        bytes.extend_from_slice(&self.id.to_le_bytes());
        bytes.extend_from_slice(&self.value.to_le_bytes());
        bytes.extend_from_slice(&self.time_signed.to_le_bytes());
        sha256d::Hash::hash(&bytes).to_byte_array()
    }

    /// Whether Core signs the hash itself on `network` (testnet, "Harden Spork6") rather than a
    /// message made of the decimal id, value and time.
    // Core: spork.cpp CSporkMessage::CheckSignature
    pub fn signs_hash(network: Network) -> bool {
        network == Network::Testnet
    }

    /// `CSporkMessage::CheckSignature`: whether the compact signature recovers a public key whose
    /// `hash160` is `key_id` (`CHashSigner::VerifyHash`). `hash_signing` is
    /// [`signs_hash`](Self::signs_hash) of the network; the other networks sign
    /// `"DarkCoin Signed Message:\n"` and the decimal text `id ‖ value ‖ time`, both as
    /// compact-size strings, hashed with SHA256d.
    // Core: spork.cpp CSporkMessage::CheckSignature, messagesigner.cpp, pubkey.cpp RecoverCompact
    pub fn check_signature(&self, key_id: &[u8; 20], hash_signing: bool) -> bool {
        let digest = if hash_signing {
            self.signature_hash()
        } else {
            let text = format!("{}{}{}", self.id, self.value, self.time_signed);
            let magic = "DarkCoin Signed Message:\n";
            // Both strings are shorter than 253 bytes: one compact-size byte each.
            let mut bytes = vec![magic.len() as u8];
            bytes.extend_from_slice(magic.as_bytes());
            bytes.push(text.len() as u8);
            bytes.extend_from_slice(text.as_bytes());
            sha256d::Hash::hash(&bytes).to_byte_array()
        };
        recover_compact_key_id(&digest, &self.signature).is_some_and(|id| id == *key_id)
    }

    /// `CSporkMessage::GetHash`: the double SHA-256 of the serialization (internal byte order);
    /// the key `mapSporksByHash` is meant to hold.
    // Core: spork.cpp CSporkMessage::GetHash
    pub fn hash(&self) -> [u8; 32] {
        let mut bytes = Vec::with_capacity(20 + 1 + self.signature.len());
        bytes.extend_from_slice(&self.id.to_le_bytes());
        bytes.extend_from_slice(&self.value.to_le_bytes());
        bytes.extend_from_slice(&self.time_signed.to_le_bytes());
        // At most 65 bytes: one compact-size byte.
        bytes.push(self.signature.len() as u8);
        bytes.extend_from_slice(&self.signature);
        sha256d::Hash::hash(&bytes).to_byte_array()
    }
}

/// A parsed `sporks.dat`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SporksDat {
    /// Whether the version string was Core's. When it was not, both maps are empty.
    pub version_matched: bool,
    /// `mapSporksByHash`, keyed by the hash as the file states it (internal byte order). Core does
    /// not check it against [`SporkMessage::hash`] while loading.
    pub by_hash: BTreeMap<[u8; 32], SporkMessage>,
    /// `mapSporksActive`, keyed by spork id.
    pub active: BTreeMap<i32, SporkMessage>,
}

impl SporksDat {
    /// `CSporkManager::CheckAndRemove`, which Core runs right after a successful load: with a
    /// spork key set, an active message whose signature does not verify is dropped from both maps
    /// (from `by_hash` under its *computed* hash), and then every remaining `by_hash` entry that
    /// does not verify is dropped. `key_id` is the network's spork key id (the `hash160` of its
    /// public key); an all-zero one is Core's "not set", and nothing is checked.
    // Core: spork.cpp CSporkManager::CheckAndRemove
    pub fn check_and_remove(&self, network: Network, key_id: &[u8; 20]) -> SporksDat {
        let mut out = self.clone();
        if *key_id == [0u8; 20] {
            return out;
        }
        let hash_signing = SporkMessage::signs_hash(network);
        out.active.retain(|_, message| {
            let valid = message.check_signature(key_id, hash_signing);
            if !valid {
                out.by_hash.remove(&message.hash());
            }
            valid
        });
        out.by_hash.retain(|_, message| message.check_signature(key_id, hash_signing));
        out
    }
}

impl Payload for SporksDat {
    const FILE: &'static str = WHAT;
    const MAGIC_MESSAGE: &'static str = "magicSporkCache";
    const VERSION_STRINGS: &'static [&'static str] = &[SERIALIZATION_VERSION_STRING];

    // Core: spork.h SporkStore::Unserialize
    fn read(&mut self, r: &mut Reader<'_>) -> FormatResult<()> {
        self.version_matched = r.byte_vec()? == SERIALIZATION_VERSION_STRING.as_bytes();
        if !self.version_matched {
            return Ok(());
        }
        let n = flat::count(r)?;
        for _ in 0..n {
            let hash = r.hash256()?;
            let message = SporkMessage::read(r)?;
            self.by_hash.entry(hash).or_insert(message);
        }
        let n = flat::count(r)?;
        for _ in 0..n {
            let id = r.i32_le()?;
            let message = SporkMessage::read(r)?;
            self.active.entry(id).or_insert(message);
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.by_hash.clear();
        self.active.clear();
    }
}

/// Parses a `sporks.dat` of `network`.
// Core: flat-database.h CFlatDB<SporkStore>::CoreRead
pub fn parse(bytes: &[u8], network: Network) -> FormatResult<Loaded<SporksDat>> {
    flat::load(bytes, network, MAX_SPORKS_DAT_SIZE)
}

/// `CPubKey::RecoverCompact` followed by `GetID()`: the `hash160` of the recovered public key,
/// serialized compressed or not as the header byte says. The header is masked with `(b - 27) & 3`
/// and `& 4` without a range check, as Core does.
// Core: pubkey.cpp CPubKey::RecoverCompact
fn recover_compact_key_id(digest: &[u8; 32], signature: &[u8]) -> Option<[u8; 20]> {
    let signature: &[u8; 65] = signature.try_into().ok()?;
    let header = i32::from(signature[0]) - 27;
    let recovery_id = RecoveryId::try_from(header & 3).ok()?;
    let compressed = header & 4 != 0;
    let recoverable = RecoverableSignature::from_compact(&signature[1..], recovery_id).ok()?;
    let public_key = recoverable.recover_ecdsa(Message::from_digest(*digest)).ok()?;
    let id = if compressed {
        hash160::Hash::hash(&public_key.serialize()).to_byte_array()
    } else {
        hash160::Hash::hash(&public_key.serialize_uncompressed()).to_byte_array()
    };
    Some(id)
}

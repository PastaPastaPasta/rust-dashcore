//! The spork manager (SP0): Core's `CSporkManager` (`spork.{h,cpp}`) over the shared spork core
//! [`dash_evo::spork`].
//!
//! - [`SporkManager`]: the spork key, the signer, the store and Core's value rules against the
//!   adjusted time; it is the node's [`SporkView`] (SB4 D29).
//! - [`file`]: the store's file, `sporks.dat` in rdashd's own format (A1 §2.9), loaded at init
//!   step 7a and written at shutdown with Core's `CFlatDB` outcomes.
//! - `handler` (feature `net`): the `spork` and `getsporks` handler P2a-core's registry
//!   dispatches to, and `getdata` for `MSG_SPORK`.
//!
//! v24 only (DEC-82): one key from `-sporkaddr` or the network's address
//! (`ChainParams::spork_signers`), the five `sporkDefs`, and `getsporks` deduplicated.

pub mod file;
#[cfg(feature = "net")]
pub mod handler;

use std::sync::Arc;

use dash_chain::SporkView;
use dash_clock::AdjustedTime;
use dash_evo::spork::{
    self, InvalidSpork, KeyId, Processed, SigForm, SporkHash, SporkId, SporkStore,
};
use dash_p2p_core::Spork;
use dash_params::ChainParams;
use dash_pkc::ecdsa::EcdsaSkBytes;
use parking_lot::RwLock;
use secp256k1::SecretKey;

pub use file::LoadError;
#[cfg(feature = "net")]
pub use handler::SporkHandler;

/// `uint256::ToString()`: the bytes reversed, in hex.
pub(crate) fn hash_hex(hash: &SporkHash) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// The spork signer (`-sporkkey`): a key whose ID is the spork key's.
struct Signer {
    key: SecretKey,
    compressed: bool,
}

/// Why `SetPrivKey` refuses a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignerError {
    /// `"Failed to parse private key"`: not the network's WIF, or not a valid scalar.
    Parse,
    /// `"New private key does not belong to spork address"`.
    NotTheSporkKey,
}

#[derive(Default)]
struct State {
    store: SporkStore,
    /// `sporkPubKeyID`; `None` is Core's null key, under which every spork is invalid.
    key_id: Option<KeyId>,
    signer: Option<Signer>,
}

/// Core's `CSporkManager`.
pub struct SporkManager {
    /// `!Params().IsTestChain()`: `GetSporkValue` hard-codes every spork (O17).
    hardened: bool,
    form: SigForm,
    /// The network's WIF version byte (`SECRET_KEY`), for `-sporkkey`.
    secret_prefix: u8,
    time: Arc<dyn AdjustedTime>,
    state: RwLock<State>,
}

impl SporkManager {
    /// The manager after Core's init: `SetSporkAddress` with the address `-sporkaddr` or the
    /// network names (`ChainParams::spork_signers`, which refuses an invalid one at startup).
    /// `time` is Core's `GetAdjustedTime()`.
    // Core: init.cpp:1757-1767
    pub fn new(params: &ChainParams, time: Arc<dyn AdjustedTime>) -> SporkManager {
        debug_assert!(params.spork_signers.key_ids.len() <= 1, "v24 has one spork key (DEC-82)");
        SporkManager {
            hardened: !params.is_test_chain,
            form: SigForm::of(params),
            secret_prefix: params.addr_params.secret_key,
            time,
            state: RwLock::new(State {
                // An all-zero ID is Core's `IsNull()` key: no spork verifies and `CheckAndRemove`
                // keeps the store.
                key_id: params
                    .spork_signers
                    .key_ids
                    .last()
                    .map(|k| *k.as_bytes())
                    .filter(|id| *id != [0; 20]),
                ..State::default()
            }),
        }
    }

    /// The signature form the network's spork key uses.
    pub fn form(&self) -> SigForm {
        self.form
    }

    /// `sporkPubKeyID`.
    pub fn key_id(&self) -> Option<KeyId> {
        self.state.read().key_id
    }

    fn adjusted_now(&self) -> i64 {
        self.time.now().0
    }

    /// Core's `SetPrivKey` (`-sporkkey`): `wif` is the network's WIF of a key whose ID is the
    /// spork key's (Core's `CMessageSigner::GetKeysFromSecret`, `DecodeSecret`).
    // Core: spork.cpp:232-256; key_io.cpp:212-229 DecodeSecret
    pub fn set_priv_key(&self, wif: &str) -> Result<(), SignerError> {
        let decoded =
            EcdsaSkBytes::from_wif(wif).filter(|(_, prefix)| *prefix == self.secret_prefix);
        let key = decoded.and_then(|(sk, _)| {
            Some((SecretKey::from_secret_bytes(*sk.to_bytes()).ok()?, sk.is_compressed()))
        });
        let Some((key, compressed)) = key else {
            tracing::info!("CSporkManager::SetPrivKey -- Failed to parse private key");
            return Err(SignerError::Parse);
        };
        self.set_signer(key, compressed)
    }

    /// Core's `SetPrivKey` with the key already decoded. The key's ID must be the spork key's.
    /// Core's test signing cannot fail with a valid key.
    // Core: spork.cpp:241-255
    pub fn set_signer(&self, key: SecretKey, compressed: bool) -> Result<(), SignerError> {
        let id = spork::key_id_of(&key.public_key(), compressed);
        let mut st = self.state.write();
        if st.key_id != Some(id) {
            tracing::info!(
                "CSporkManager::SetPrivKey -- New private key does not belong to spork address"
            );
            return Err(SignerError::NotTheSporkKey);
        }
        tracing::info!("CSporkManager::SetPrivKey -- Successfully initialized as spork signer");
        st.signer = Some(Signer {
            key,
            compressed,
        });
        Ok(())
    }

    /// Core's `UpdateSpork` (the `sporkupdate` RPC): signs `value` for `id` at the adjusted time
    /// and stores it. The caller relays the returned hash (`RelayInv`). `None` without a signer.
    // Core: spork.cpp:138-161
    pub fn update_spork(&self, id: SporkId, value: i64) -> Option<SporkHash> {
        let time_signed = self.adjusted_now();
        let mut st = self.state.write();
        let Some(signer) = &st.signer else {
            tracing::info!("CSporkMessage::Sign -- signing key is not valid");
            tracing::info!(
                "CSporkManager::UpdateSpork -- ERROR: signing failed for spork {}",
                id.0
            );
            return None;
        };
        let msg = spork::sign(id, value, time_signed, &signer.key, signer.compressed, self.form);
        let hash = spork::spork_hash(&msg);
        tracing::info!("CSporkManager::UpdateSpork -- signed {} {}", id.0, hash_hex(&hash));
        st.store.insert(msg);
        Some(hash)
    }

    /// Core's `IsValidSpork` at the adjusted time. The key is copied out and the signature
    /// checked without the lock, as Core does.
    // Core: spork.cpp:86-102
    pub fn is_valid_spork(&self, msg: &Spork) -> Result<(), InvalidSpork> {
        let key_id = self.state.read().key_id;
        let r = spork::validate(msg, self.adjusted_now(), key_id.as_ref(), self.form);
        match r {
            Err(InvalidSpork::TooFarInFuture) => {
                tracing::debug!(target: "spork", "CSporkManager::IsValidSpork -- ERROR: too far into the future");
            }
            Err(InvalidSpork::InvalidSignature) => {
                tracing::debug!(target: "spork", "CSporkManager::IsValidSpork -- ERROR: invalid signature");
            }
            Ok(()) => {}
        }
        r
    }

    /// Core's `ProcessSpork` for a valid spork; `peer` is the source for the log line.
    // Core: spork.cpp:104-125
    pub fn process_spork(&self, msg: Spork, peer: Option<i64>) -> Processed {
        let suffix = peer.map(|p| format!(" peer={p}")).unwrap_or_default();
        let line = format!(
            "SPORK -- hash: {} id: {} value: {:>10}{suffix}",
            hash_hex(&spork::spork_hash(&msg)),
            msg.spork_id,
            msg.value
        );
        let outcome = self.state.write().store.process(msg);
        match outcome {
            Processed::Seen => tracing::debug!(target: "spork", "{line} seen"),
            Processed::Updated => tracing::info!("{line} updated"),
            Processed::New => tracing::info!("{line} new"),
        }
        outcome
    }

    /// Core's `ActiveSporks`, in Core's order.
    pub fn active_sporks(&self) -> Vec<Spork> {
        self.state.read().store.active().to_vec()
    }

    /// Core's `GetSporkByHash`.
    pub fn spork_by_hash(&self, hash: &SporkHash) -> Option<Spork> {
        self.state.read().store.by_hash(hash).cloned()
    }

    /// Core's `GetSporkValue`.
    pub fn value(&self, id: SporkId) -> i64 {
        let value = self.state.read().store.value(id, self.hardened);
        if !self.hardened
            && spork::def_of(id).is_none()
            && self.state.read().store.active_value(id).is_none()
        {
            tracing::debug!(target: "spork", "CSporkManager::GetSporkValue -- Unknown Spork ID {}", id.0);
        }
        value
    }

    /// Core's `IsSporkActive`.
    // Core: spork.cpp:163-169
    pub fn is_active(&self, id: SporkId) -> bool {
        spork::is_active(self.value(id), self.adjusted_now())
    }

    /// `IsSporkActive(SPORK_2_INSTANTSEND_ENABLED)`, the spork half of Core's
    /// `CInstantSendManager::IsInstantSendEnabled`.
    // Core: instantsend/instantsend.cpp:510-513
    pub fn instantsend_enabled(&self) -> bool {
        self.is_active(SporkId::SPORK_2_INSTANTSEND_ENABLED)
    }

    /// Core's `SporkStore::ToString`.
    pub fn summary(&self) -> String {
        format!("Sporks: {}", self.state.read().store.active().len())
    }

    /// Core's `CheckAndRemove`.
    pub fn check_and_remove(&self) {
        let mut st = self.state.write();
        let key_id = st.key_id;
        st.store.check_and_remove(key_id.as_ref(), self.form);
    }
}

impl SporkView for SporkManager {
    // Core: chainlock/chainlock.cpp:33 Chainlocks::IsEnabled
    fn chainlocks_enabled(&self) -> bool {
        self.is_active(SporkId::SPORK_19_CHAINLOCKS_ENABLED)
    }
}

#[cfg(test)]
mod tests;

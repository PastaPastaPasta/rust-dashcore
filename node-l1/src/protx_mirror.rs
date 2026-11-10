//! A mirror of base-sdk's provider payloads (DEC-1066, BL-652): plain structs with public fields,
//! the signature fields included, for the oracle comparisons and the structure-aware mutators
//! that write payload fields, signed or not.
//!
//! base-sdk keeps these fields private behind getters. Its `new`s reset the signature of
//! [`ProRegTx`], [`ProUpServTx`], [`ProUpRegTx`] and [`ProUpRevTx`], and it sets a signature only
//! through the verifying `Signable::set_signature`, so a generated or mutated payload that keeps a
//! signature it does not verify against cannot be built there. A mutator writes the mirror
//! instead, and the mirror's bytes are what reach Core and the node.
//!
//! The rules (DEC-1066):
//! - The mirror lives in node-l1 (test-only, never a runtime dependency) and its fuzz and test
//!   support. No crate the node binary links uses it.
//! - Each type has the same fields, in the same order and with the same field types, as the
//!   base-sdk payload of its name at the pinned revision, and getters of the same names and return
//!   types as base-sdk's (`get_copy` gives the value, `get` a reference), so code ported to the
//!   getters reads either. [`ProUpRegTx::set_version`] and [`ProUpRevTx::set_version`] are base-sdk's
//!   setters.
//! - Its encoder is base-sdk's, field by field: each field through its own type's `BaseCodec` (or
//!   the BLS serialization base-sdk's private helper calls), in the order and under the version
//!   conditions of base-sdk's `encode`. Each `encode` names the base-sdk source it follows. The
//!   round-trip test (`protx_mirror/tests.rs`) checks it byte for byte against base-sdk's encoder
//!   on every valid payload of these types in the reachable vectors.
//! - Two of base-sdk's ProRegTx digests are copied the same way, with the private helpers they
//!   call: [`ProRegTx::sign_string`] and [`ProRegTx::shared_consent_bytes`]. So a ProTx signer
//!   signs the payload as written, not base-sdk's reading of its bytes (DEC-1066 decision 1). The
//!   tests check both against base-sdk's on the same vectors.
//! - base-sdk to mirror: [`From`] through base-sdk's getters, or [`BaseCodec::decode`], which is
//!   base-sdk's decode followed by that.
//! - Mirror to base-sdk only through bytes: [`ToBsdk::to_bsdk`] encodes the mirror and decodes the
//!   bytes with base-sdk. Nothing here, and nothing that uses it, builds a base-sdk payload from a
//!   mirror's fields. To audit: `grep -rn 'protx_mirror' --include=*.rs` lists every user, all in
//!   node-l1 and node-fuzz; inside this file, base-sdk payload values are made only by
//!   `<bsdk>::decode` in [`ToBsdk::to_bsdk`] and [`BaseCodec::decode`].
//!
//! [`ProRegTx`]: crate::protx_mirror::ProRegTx
//! [`ProUpServTx`]: crate::protx_mirror::ProUpServTx
//! [`ProUpRegTx`]: crate::protx_mirror::ProUpRegTx
//! [`ProUpRevTx`]: crate::protx_mirror::ProUpRevTx
//! [`ProUpRegTx::set_version`]: crate::protx_mirror::ProUpRegTx::set_version
//! [`ProUpRevTx::set_version`]: crate::protx_mirror::ProUpRevTx::set_version
//! [`ProRegTx::sign_string`]: crate::protx_mirror::ProRegTx::sign_string
//! [`ProRegTx::shared_consent_bytes`]: crate::protx_mirror::ProRegTx::shared_consent_bytes
//! [`ToBsdk::to_bsdk`]: crate::protx_mirror::ToBsdk::to_bsdk
//! [`BaseCodec::decode`]: dash_types::codec::BaseCodec::decode

use bitcoin_hashes::sha256d;
use bitcoin_primitives::script::ScriptPubKeyBuf;
use dash_num::Hash256;
use dash_params::ChainSetup;
use dash_pkc::bls::{BlsEagerSig, BlsLazyPk, BlsSchemeKind};
use dash_pkc::ecdsa::EcdsaPkHash as PubKeyHash;
use dash_primitives as bsdk;
use dash_primitives::{
    CollateralShare, CollateralSig, DissolveSig, InputsHash, JoinSig, MnType, NetInfo, OwnerSig,
    PayoutShare, PlatformNodeId, ProTxVersion, RevocationReason, SharedRegistrarSig, Transaction,
    TxHash, protx_bls_scheme,
};
use dash_script::Recipient;
use dash_types::Numeric;
use dash_types::codec::{BaseCodec, DecodeError, EncodeBuf};
use hex_conservative::DisplayHex;

#[cfg(test)]
mod tests;

/// Declares a mirror struct: its public fields, base-sdk's getters for them (`copy` for a
/// `get_copy` getter, `get` for a `get` one) and [`From`] the base-sdk payload of the same name,
/// read through those getters.
macro_rules! mirror {
    (
        $(#[$attr:meta])*
        $name:ident { $($kind:ident $field:ident: $ty:ty),+ $(,)? }
    ) => {
        $(#[$attr])*
        #[derive(Clone, Debug, PartialEq)]
        pub struct $name {
            $(pub $field: $ty,)+
        }

        impl $name {
            $(mirror!(@getter $kind $field $ty);)+
        }

        impl From<&bsdk::$name> for $name {
            fn from(p: &bsdk::$name) -> Self {
                Self { $($field: mirror!(@read $kind p $field),)+ }
            }
        }

        impl From<bsdk::$name> for $name {
            fn from(p: bsdk::$name) -> Self {
                Self::from(&p)
            }
        }

        impl BaseCodec for $name {
            /// base-sdk's decode, then [`From`].
            fn decode(data: &mut &[u8]) -> Result<Self, DecodeError> {
                bsdk::$name::decode(data).map(Self::from)
            }

            fn encode(&self, buf: &mut impl EncodeBuf) {
                self.encode_payload(buf);
            }
        }

        impl ToBsdk for $name {
            type Bsdk = bsdk::$name;
        }
    };
    (@getter copy $field:ident $ty:ty) => {
        #[must_use]
        pub fn $field(&self) -> $ty {
            self.$field
        }
    };
    (@getter get $field:ident $ty:ty) => {
        #[must_use]
        pub fn $field(&self) -> &$ty {
            &self.$field
        }
    };
    (@read copy $p:ident $field:ident) => {
        $p.$field()
    };
    (@read get $p:ident $field:ident) => {
        $p.$field().clone()
    };
}

/// The way back to base-sdk: the mirror's bytes, decoded by base-sdk.
pub trait ToBsdk: BaseCodec {
    /// The base-sdk payload this type mirrors.
    type Bsdk: BaseCodec;

    /// The mirror's encoding.
    #[must_use]
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    /// base-sdk's decode of [`ToBsdk::to_bytes`]; `None` when it fails or leaves bytes over.
    /// base-sdk's decode reads some values as others (a domain entry with a bad name as an
    /// invalid entry, for one), so the result can differ from the mirror; the caller decides
    /// whether that matters.
    #[must_use]
    fn to_bsdk(&self) -> Option<Self::Bsdk> {
        let bytes = self.to_bytes();
        let mut data = bytes.as_slice();
        let p = Self::Bsdk::decode(&mut data).ok()?;
        data.is_empty().then_some(p)
    }
}

mirror! {
    /// base-sdk's `ProRegTx` (type 1).
    ProRegTx {
        copy version: ProTxVersion,
        copy mn_type: MnType,
        copy mode: u16,
        copy collateral_hash: TxHash,
        copy collateral_index: u32,
        get net_info: NetInfo,
        copy key_id_owner: PubKeyHash,
        get pub_key_operator: BlsLazyPk,
        copy key_id_voting: PubKeyHash,
        copy operator_reward: u16,
        get script_payout: ScriptPubKeyBuf,
        get payouts: Vec<PayoutShare>,
        get shares: Vec<CollateralShare>,
        get join_sigs: Vec<JoinSig>,
        copy early_period_blocks: u32,
        copy early_penalty: i64,
        copy inputs_hash: InputsHash,
        copy platform_node_id: Option<PlatformNodeId>,
        copy platform_p2p_port: Option<u16>,
        copy platform_http_port: Option<u16>,
        get sig: CollateralSig,
    }
}

mirror! {
    /// base-sdk's `ProUpServTx` (type 2).
    ProUpServTx {
        copy version: ProTxVersion,
        copy mn_type: MnType,
        copy pro_tx_hash: TxHash,
        get net_info: NetInfo,
        get script_operator_payout: ScriptPubKeyBuf,
        copy inputs_hash: InputsHash,
        copy platform_node_id: Option<PlatformNodeId>,
        copy platform_p2p_port: Option<u16>,
        copy platform_http_port: Option<u16>,
        get sig: BlsEagerSig,
    }
}

mirror! {
    /// base-sdk's `ProUpRegTx` (type 3).
    ProUpRegTx {
        copy version: ProTxVersion,
        copy pro_tx_hash: TxHash,
        copy mode: u16,
        get pub_key_operator: BlsLazyPk,
        copy key_id_voting: PubKeyHash,
        get script_payout: ScriptPubKeyBuf,
        get payouts: Vec<PayoutShare>,
        copy inputs_hash: InputsHash,
        get sig: OwnerSig,
    }
}

mirror! {
    /// base-sdk's `ProUpRevTx` (type 4).
    ProUpRevTx {
        copy version: ProTxVersion,
        copy pro_tx_hash: TxHash,
        copy reason: RevocationReason,
        copy inputs_hash: InputsHash,
        get sig: BlsEagerSig,
    }
}

mirror! {
    /// base-sdk's `ProDisTx` (type 10).
    ProDisTx {
        copy version: u16,
        copy pro_tx_hash: TxHash,
        copy actor_index: u16,
        get sigs: Vec<DissolveSig>,
    }
}

mirror! {
    /// base-sdk's `ProUpShareTx` (type 11).
    ProUpShareTx {
        copy version: u16,
        copy pro_tx_hash: TxHash,
        copy share_index: u16,
        get script_reward: ScriptPubKeyBuf,
        copy inputs_hash: InputsHash,
        get sig: Vec<u8>,
    }
}

mirror! {
    /// base-sdk's `ProUpSharedRegTx` (type 12).
    ProUpSharedRegTx {
        copy version: u16,
        copy pro_tx_hash: TxHash,
        get pub_key_operator: BlsLazyPk,
        copy key_id_voting: PubKeyHash,
        copy inputs_hash: InputsHash,
        get sigs: Vec<SharedRegistrarSig>,
    }
}

// The encoders. Each follows base-sdk's at fda80b21, file and function named; the private helpers
// it calls are copied below with their sources.

impl ProRegTx {
    /// `pkgs/primitives/src/provider/proregtx.rs`: `encode` (`encode_unsigned`, then `sig`).
    fn encode_payload(&self, buf: &mut impl EncodeBuf) {
        self.encode_unsigned(buf);
        self.sig.encode(buf);
    }

    /// `proregtx.rs` `encode_unsigned`, base-sdk's `sig_hash_bytes`.
    fn encode_unsigned(&self, buf: &mut impl EncodeBuf) {
        self.version.encode(buf);
        self.mn_type.to_base().encode(buf);
        self.mode.encode(buf);
        self.collateral_hash.encode(buf);
        self.collateral_index.encode(buf);
        write_net_info(buf, &self.net_info, self.version);
        self.key_id_owner.encode(buf);
        write_lazy(&self.pub_key_operator, protx_bls_scheme(self.version), buf);
        self.key_id_voting.encode(buf);
        self.operator_reward.encode(buf);
        if self.version >= ProTxVersion::CoreV24 {
            write_list(&self.payouts, buf);
            write_list(&self.shares, buf);
            write_items(&self.join_sigs, buf);
            self.early_period_blocks.encode(buf);
            self.early_penalty.encode(buf);
        } else {
            self.script_payout.encode(buf);
        }
        self.inputs_hash.encode(buf);
        write_platform_fields(
            buf,
            self.mn_type,
            self.version,
            &self.platform_node_id,
            self.platform_p2p_port,
            self.platform_http_port,
        );
    }

    // base-sdk's `sign_string`, copied (DEC-1066 decision 1), so the sign string is of the
    // payload as written: base-sdk's needs a base-sdk payload, and the mirror reaches one only
    // through base-sdk's decode, which refuses or reads otherwise some of the payloads node-l1
    // writes. Source: fda80b21 `pkgs/primitives/src/provider/proregtx.rs:404-429`.
    /// Core's `MakeSignString`, which the key of an external collateral signs as a text message:
    /// the payout, the operator reward, the owner and voting addresses and the payload hash,
    /// separated by `|`. It depends on the network through the address prefixes of `setup`
    /// (base-sdk's takes them as `setup.base_sdk_addr_params()`).
    #[must_use]
    pub fn sign_string(&self, setup: &ChainSetup) -> String {
        let addr = setup.base_sdk_addr_params();
        let mut unsigned = Vec::new();
        self.encode_unsigned(&mut unsigned);
        let hash = hash256(&unsigned);
        let payout = if self.version >= ProTxVersion::CoreV24 {
            let entries: Vec<String> = self
                .payouts
                .iter()
                .map(|payout| {
                    format!(
                        "{}:{}",
                        destination_string(payout.script_payout().as_bytes(), setup),
                        payout.reward()
                    )
                })
                .collect();
            entries.join(",")
        } else {
            destination_string(self.script_payout.as_bytes(), setup)
        };
        format!(
            "{payout}|{}|{}|{}|{}",
            self.operator_reward,
            self.key_id_owner.to_base58c(addr.pubkey_addr),
            self.key_id_voting.to_base58c(addr.pubkey_addr),
            hash,
        )
    }

    // base-sdk's `shared_consent_bytes`, copied for the same reason as `sign_string`. Source:
    // fda80b21 `pkgs/primitives/src/provider/proregtx.rs:443-475`.
    /// The bytes whose SHA256d each share owner signs to consent to a shared registration (v24),
    /// Core's `MakeSharedRegConsentHash`: the funding inputs and their sequences, every output, the
    /// share table, the penalty terms and the registrar configuration of `tx`, the transaction
    /// that carries this payload.
    #[must_use]
    pub fn shared_consent_bytes(&self, tx: &Transaction) -> Vec<u8> {
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for input in tx.inputs() {
            input.prevout().encode(&mut inputs);
        }
        for output in tx.outputs() {
            output.encode(&mut outputs);
        }

        let mut buf = Vec::new();
        String::from("DashSharedMNReg").encode(&mut buf);
        self.version.encode(&mut buf);
        write_tx_header(tx, &mut buf);
        hash256(&inputs).encode(&mut buf);
        for input in tx.inputs() {
            input.sequence().encode(&mut buf);
        }
        hash256(&outputs).encode(&mut buf);
        self.mn_type.to_base().encode(&mut buf);
        self.mode.encode(&mut buf);
        write_net_info(&mut buf, &self.net_info, self.version);
        if self.mn_type == MnType::Evo {
            self.platform_node_id.unwrap_or_default().encode(&mut buf);
        }
        self.key_id_voting.encode(&mut buf);
        write_lazy(&self.pub_key_operator, protx_bls_scheme(self.version), &mut buf);
        self.operator_reward.encode(&mut buf);
        write_list(&self.shares, &mut buf);
        self.early_period_blocks.encode(&mut buf);
        self.early_penalty.encode(&mut buf);
        buf
    }
}

impl ProUpServTx {
    /// `pkgs/primitives/src/provider/proupservtx.rs`: `encode` (`encode_unsigned`, then `sig`
    /// under the version's scheme).
    fn encode_payload(&self, buf: &mut impl EncodeBuf) {
        self.encode_unsigned(buf);
        self.sig.to_bytes(protx_bls_scheme(self.version)).encode(buf);
    }

    /// `proupservtx.rs` `encode_unsigned`.
    fn encode_unsigned(&self, buf: &mut impl EncodeBuf) {
        self.version.encode(buf);
        if self.version >= ProTxVersion::BasicBls {
            self.mn_type.to_base().encode(buf);
        }
        self.pro_tx_hash.encode(buf);
        write_net_info(buf, &self.net_info, self.version);
        self.script_operator_payout.encode(buf);
        self.inputs_hash.encode(buf);
        write_platform_fields(
            buf,
            self.mn_type,
            self.version,
            &self.platform_node_id,
            self.platform_p2p_port,
            self.platform_http_port,
        );
    }
}

impl ProUpRegTx {
    /// base-sdk's setter (`#[getset(set = "pub")]`).
    pub fn set_version(&mut self, version: ProTxVersion) -> &mut Self {
        self.version = version;
        self
    }

    /// `pkgs/primitives/src/provider/proupregtx.rs`: `encode` (`encode_unsigned`, then `sig`).
    fn encode_payload(&self, buf: &mut impl EncodeBuf) {
        self.encode_unsigned(buf);
        self.sig.encode(buf);
    }

    /// `proupregtx.rs` `encode_unsigned`.
    fn encode_unsigned(&self, buf: &mut impl EncodeBuf) {
        self.version.encode(buf);
        self.pro_tx_hash.encode(buf);
        self.mode.encode(buf);
        write_lazy(&self.pub_key_operator, protx_bls_scheme(self.version), buf);
        self.key_id_voting.encode(buf);
        if self.version >= ProTxVersion::CoreV24 {
            write_list(&self.payouts, buf);
        } else {
            self.script_payout.encode(buf);
        }
        self.inputs_hash.encode(buf);
    }
}

impl ProUpRevTx {
    /// base-sdk's setter (`#[getset(set = "pub")]`).
    pub fn set_version(&mut self, version: ProTxVersion) -> &mut Self {
        self.version = version;
        self
    }

    /// `pkgs/primitives/src/provider/prouprevtx.rs`: `encode` (`encode_unsigned`, then `sig`
    /// under the version's scheme).
    fn encode_payload(&self, buf: &mut impl EncodeBuf) {
        self.encode_unsigned(buf);
        self.sig.to_bytes(protx_bls_scheme(self.version)).encode(buf);
    }

    /// `prouprevtx.rs` `encode_unsigned`.
    fn encode_unsigned(&self, buf: &mut impl EncodeBuf) {
        self.version.encode(buf);
        self.pro_tx_hash.encode(buf);
        self.reason.encode(buf);
        self.inputs_hash.encode(buf);
    }
}

impl ProDisTx {
    /// `pkgs/primitives/src/provider/prodistx.rs`: `encode`.
    fn encode_payload(&self, buf: &mut impl EncodeBuf) {
        self.version.encode(buf);
        self.pro_tx_hash.encode(buf);
        self.actor_index.encode(buf);
        write_list(&self.sigs, buf);
    }
}

impl ProUpShareTx {
    /// `pkgs/primitives/src/provider/proupsharetx.rs`: `codec_type!`'s `encode`
    /// (`pkgs/primitives/src/codec.rs` `codec_base!`), every field in declaration order.
    fn encode_payload(&self, buf: &mut impl EncodeBuf) {
        self.encode_unsigned(buf);
        self.sig.encode(buf);
    }

    /// `proupsharetx.rs` `sig_hash_bytes`: every field but `sig`.
    fn encode_unsigned(&self, buf: &mut impl EncodeBuf) {
        self.version.encode(buf);
        self.pro_tx_hash.encode(buf);
        self.share_index.encode(buf);
        self.script_reward.encode(buf);
        self.inputs_hash.encode(buf);
    }
}

impl ProUpSharedRegTx {
    /// `pkgs/primitives/src/provider/proupsharedregtx.rs`: `encode` (`encode_unsigned`, then the
    /// signature list).
    fn encode_payload(&self, buf: &mut impl EncodeBuf) {
        self.encode_unsigned(buf);
        write_list(&self.sigs, buf);
    }

    /// `proupsharedregtx.rs` `encode_unsigned`: the operator key always under the basic scheme.
    fn encode_unsigned(&self, buf: &mut impl EncodeBuf) {
        self.version.encode(buf);
        self.pro_tx_hash.encode(buf);
        write_lazy(&self.pub_key_operator, BlsSchemeKind::Basic, buf);
        self.key_id_voting.encode(buf);
        self.inputs_hash.encode(buf);
    }
}

/// `SER_GETHASH`, base-sdk's `sig_hash_bytes`: the payload without its signature, which
/// dash-dml's `payload_sig_hash` hashes. The mirror's own encoding, so a signature over a payload
/// base-sdk's decode would read differently still covers the payload's bytes.
macro_rules! signed_payload {
    ($($t:ty),+) => {
        $(impl dash_dml::specialtx::sig::SignedPayload for $t {
            fn sig_hash_bytes(&self) -> Option<Vec<u8>> {
                let mut buf = Vec::new();
                self.encode_unsigned(&mut buf);
                Some(buf)
            }
        })+
    };
}

signed_payload!(ProRegTx, ProUpServTx, ProUpRegTx, ProUpRevTx, ProUpShareTx, ProUpSharedRegTx);

// base-sdk's private helpers, copied.

/// `pkgs/primitives/src/provider/mod.rs` `write_net_info`: the network info the payload version
/// selects; a variant the version does not select writes nothing.
fn write_net_info(buf: &mut impl EncodeBuf, net_info: &NetInfo, version: ProTxVersion) {
    if version >= ProTxVersion::CoreV24 {
        if let NetInfo::Extended(ext) = net_info {
            ext.encode(buf);
        }
    } else if let NetInfo::Legacy(svc) = net_info {
        svc.encode(buf);
    }
}

/// `pkgs/primitives/src/provider/mod.rs` `write_platform_fields`: an Evo node's node id, and its
/// ports below ExtAddr, absent ones as their defaults.
fn write_platform_fields(
    buf: &mut impl EncodeBuf,
    mn_type: MnType,
    version: ProTxVersion,
    platform_node_id: &Option<PlatformNodeId>,
    platform_p2p_port: Option<u16>,
    platform_http_port: Option<u16>,
) {
    if mn_type == MnType::Evo {
        platform_node_id.unwrap_or_default().encode(buf);
        if version < ProTxVersion::CoreV24 {
            platform_p2p_port.unwrap_or(0).encode(buf);
            platform_http_port.unwrap_or(0).encode(buf);
        }
    }
}

/// `pkgs/primitives/src/payload/bls.rs` `write_lazy` (`lazy_bytes`): the key's object under
/// `scheme`, zeros when invalid, resolved on a copy so the field stays as it was.
fn write_lazy(field: &BlsLazyPk, scheme: BlsSchemeKind, buf: &mut impl EncodeBuf) {
    field.clone().get().to_bytes(scheme).encode(buf);
}

/// `pkgs/primitives/src/provider/shares.rs` `write_items`: the items with no count.
fn write_items<T: BaseCodec>(items: &[T], buf: &mut impl EncodeBuf) {
    for item in items {
        item.encode(buf);
    }
}

/// `shares.rs` `write_list`: a one-byte count, cast as Core casts it (a count above 255 wraps),
/// and every item.
#[allow(clippy::cast_possible_truncation)] // Core's `uint8_t` cast, as base-sdk's
fn write_list<T: BaseCodec>(items: &[T], buf: &mut impl EncodeBuf) {
    (items.len() as u8).encode(buf);
    write_items(items, buf);
}

// The private helpers of base-sdk's `sign_string` and `shared_consent_bytes`, copied from fda80b21
// `pkgs/primitives/src/payload/sighash.rs`.

/// `sighash.rs:38-41` `hash256`: SHA256d, Core's `Hash` over a serialization.
fn hash256(bytes: &[u8]) -> Hash256 {
    Hash256::from_lendian(sha256d::Hash::hash(bytes).to_byte_array())
}

/// `sighash.rs:43-49` `destination_string`: a script as Core's provider payloads print it into
/// signed strings, the address of the destination `ExtractDestination` finds, else the hex.
/// It takes the setup where base-sdk's takes its address prefixes, which are
/// `setup.base_sdk_addr_params()`.
fn destination_string(script: &[u8], setup: &ChainSetup) -> String {
    Recipient::from_script(script)
        .and_then(|recipient| recipient.to_base58c(&setup.base_sdk_addr_params()))
        .unwrap_or_else(|| script.to_lower_hex_string())
}

/// `sighash.rs:95-101` `write_tx_header`: the fields of `tx` that Core's shared-masternode digests
/// take directly, the version, the type and the lock time.
fn write_tx_header(tx: &Transaction, buf: &mut impl EncodeBuf) {
    tx.version().encode(buf);
    tx.tx_type().to_base().encode(buf);
    tx.lock_time().encode(buf);
}

//! Test builders (feature `test-utils`), for this crate's tests and its consumers' (EV2 §2.1
//! `testkit`): simple masternodes, plus `dash_evo`'s block tree and proTxHashes. EV2a adds its
//! list builders and a synthetic block generator here. Lists are filled with
//! `MnList::insert_for_tests`.

use std::sync::Arc;

use dash_consensus::primitives::CoreWrite as _;
use dash_pkc::bls::{
    BlsEagerSig, BlsLazyPk, BlsScIetf, BlsSchemeKind, BlsSchemeSource, BlsSecretKey, BlsSigId,
};
use dash_primitives::{NetInfo, NetInfoV1, OutPoint, PlatformNodeId, TxHash};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;

pub use dash_evo::testkit::{MemChain, pro_tx};

pub mod replay;
pub mod window;

use crate::{ColdDerived, Dmn, MnCold, MnHot, MnType, OperatorKey};

/// A version-1 state registered at height 100, unconfirmed, with an empty legacy service, a null
/// legacy operator key, owner and voting key ids `[n; 20]`, and for Evo a platformNodeID
/// `[0x80 | n; 20]` (so it never shares a slot with the key ids).
pub fn cold_state(n: u8, mn_type: MnType) -> MnCold {
    let mut no_service: &[u8] = &[0; 18];
    let service = NetInfoV1::decode(&mut no_service).expect("18 bytes are a legacy service");
    let platform_node_id = match mn_type {
        MnType::Regular => [0u8; 20],
        MnType::Evo => [0x80 | n; 20],
    };
    MnCold {
        version: 1,
        registered_height: 100,
        revocation_reason: 0,
        confirmed_hash: Default::default(),
        confirmed_hash_with_pro_tx: Default::default(),
        key_id_owner: [n; 20].into(),
        pub_key_operator: BlsLazyPk::new(BlsSchemeKind::Legacy),
        key_id_voting: [n; 20].into(),
        net_info: Arc::new(NetInfo::Legacy(service)),
        script_payout: Vec::new(),
        payouts: Vec::new(),
        shares: Vec::new(),
        early_period_blocks: 0,
        early_penalty: 0,
        script_operator_payout: Vec::new(),
        platform_node_id: PlatformNodeId::from(platform_node_id),
        platform_p2p_port: 0,
        platform_http_port: 0,
        derived: ColdDerived::default(),
    }
}

/// Masternode `n`: [`pro_tx`]`(n)`, internal id `n`, collateral `([n; 32], 0)`, valid, with
/// [`cold_state`]`(n, mn_type)`.
pub fn dmn(n: u8, mn_type: MnType) -> Arc<Dmn> {
    Arc::new(Dmn {
        pro_tx_hash: pro_tx(n),
        internal_id: u64::from(n),
        collateral: OutPoint::new(TxHash::from([n; 32]), 0),
        operator_reward: 0,
        mn_type: mn_type.to_raw(),
        hot: MnHot::default(),
        cold: Arc::new(cold_state(n, mn_type)),
    })
}

/// A valid operator key: the public key of the secret derived from `[n; 32]`, stored under
/// `scheme` (a decoded key, as `Set(pubkey, legacy)` leaves it).
///
/// # Panics
///
/// Never: 32 bytes are enough key material.
pub fn operator_key(n: u8, scheme: BlsSchemeKind) -> OperatorKey {
    operator_key_from(&[n; 32], scheme)
}

/// [`operator_key`]`(n, Legacy)`: the key a version-1 provider payload carries.
pub fn legacy_operator_key(n: u8) -> OperatorKey {
    operator_key(n, BlsSchemeKind::Legacy)
}

/// [`operator_key`] of the secret derived from `ikm`.
///
/// # Panics
///
/// Never: 32 bytes are enough key material.
pub fn operator_key_from(ikm: &[u8; 32], scheme: BlsSchemeKind) -> OperatorKey {
    BlsLazyPk::from_value(basic_pk(ikm).get(), scheme)
}

/// The payload field of operator key `key`: the bytes a decode left it, or its value's bytes under
/// its scheme.
pub fn wire_key(key: &BlsLazyPk) -> dash_primitives::OperatorKey {
    dash_primitives::OperatorKey::from(dash_pkc::bls::BlsPkBytes::from_bytes(key.to_bytes()))
}

/// The basic-scheme signature of `hash` by the secret of [`operator_key`]`(n, _)`.
///
/// # Panics
///
/// Never: 32 bytes are enough key material, and the signature is canonical.
pub fn operator_sig(n: u8, hash: &[u8; 32]) -> BlsEagerSig {
    let secret = BlsSecretKey::<BlsScIetf>::from_ikm(&[n; 32]).expect("32 bytes of key material");
    let bytes = secret.sign_with(hash, BlsSigId::Basic).to_bytes();
    BlsEagerSig::decode(&bytes, BlsSchemeSource::Version(BlsSchemeKind::Basic)).expect("a sig")
}

/// The basic-scheme public key of the secret derived from `ikm`.
fn basic_pk(ikm: &[u8; 32]) -> BlsLazyPk {
    let secret = BlsSecretKey::<BlsScIetf>::from_ikm(ikm).expect("32 bytes of key material");
    BlsLazyPk::from_bytes(
        &secret.public_key().to_bytes(),
        BlsSchemeSource::Version(BlsSchemeKind::Basic),
    )
}

/// `dmn` with its state changed by `f`.
pub fn dmn_with(n: u8, mn_type: MnType, f: impl FnOnce(&mut Dmn, &mut MnCold)) -> Arc<Dmn> {
    let mut dmn = (*dmn(n, mn_type)).clone();
    let mut cold = (*dmn.cold).clone();
    f(&mut dmn, &mut cold);
    dmn.cold = Arc::new(fresh(cold));
    Arc::new(dmn)
}

/// `cold` with its derived caches dropped, for a state edited by hand.
pub fn fresh(mut cold: MnCold) -> MnCold {
    cold.reset_derived();
    cold
}

/// 32 bytes derived from `(i, salt)` (splitmix64): distinct for distinct inputs, spread over the
/// whole range like real hashes.
fn spread(i: u32, salt: u64) -> [u8; 32] {
    let mut state = u64::from(i) ^ salt.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut out = [0u8; 32];
    for chunk in out.chunks_mut(8) {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        chunk.copy_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out
}

/// The key material of [`numbered`]`(i)`'s operator key: its secret is
/// `BlsSecretKey::from_ikm` of it (for harnesses that sign as the masternode).
pub fn numbered_operator_ikm(i: u32) -> [u8; 32] {
    spread(i, 1)
}

/// A mainnet-shaped masternode `i` for lists of thousands (benches): a spread proTxHash and
/// collateral, internal id `i`, version 2 with a basic operator key derived from `i`, a routable
/// IPv4 service on port 9999, distinct owner and voting key ids, a P2PKH payout, paid and
/// confirmed; every eighth one is Evo with a platformNodeID.
///
/// # Panics
///
/// Never: 32 bytes are enough key material.
pub fn numbered(i: u32) -> Arc<Dmn> {
    use dash_primitives::{AddrV1, ServiceV1};
    let evo = i.is_multiple_of(8);
    let mn_type = if evo {
        MnType::Evo
    } else {
        MnType::Regular
    };
    let id = |salt| -> [u8; 20] { spread(i, salt)[..20].try_into().expect("20 bytes") };
    let key = basic_pk(&numbered_operator_ikm(i));
    let mut ip = [0u8; 16];
    ip[10..12].copy_from_slice(&[0xff, 0xff]);
    ip[12] = 1 + (i >> 24) as u8 % 100;
    ip[13..].copy_from_slice(&i.to_be_bytes()[1..]);
    let payout = |salt| {
        let mut script = vec![0x76, 0xa9, 0x14];
        script.extend_from_slice(&id(salt));
        script.extend_from_slice(&[0x88, 0xac]);
        script
    };
    let cold = MnCold {
        version: 2,
        registered_height: 1_000_000 + i as i32,
        revocation_reason: 0,
        confirmed_hash: spread(i, 2).into(),
        confirmed_hash_with_pro_tx: spread(i, 3).into(),
        key_id_owner: id(4).into(),
        pub_key_operator: key,
        key_id_voting: id(5).into(),
        net_info: Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
            AddrV1::from_bytes(ip),
            9999,
        )))),
        script_payout: payout(6),
        payouts: Vec::new(),
        shares: Vec::new(),
        early_period_blocks: 0,
        early_penalty: 0,
        script_operator_payout: Vec::new(),
        platform_node_id: PlatformNodeId::from(if evo {
            id(7)
        } else {
            [0; 20]
        }),
        platform_p2p_port: if evo {
            26656
        } else {
            0
        },
        platform_http_port: if evo {
            443
        } else {
            0
        },
        derived: ColdDerived::default(),
    };
    Arc::new(Dmn {
        pro_tx_hash: dash_evo::types::ProTxHash::from(spread(i, 8)),
        internal_id: u64::from(i),
        collateral: OutPoint::new(TxHash::from(spread(i, 9)), i % 3),
        operator_reward: 0,
        mn_type: mn_type.to_raw(),
        hot: MnHot {
            last_paid_height: 2_000_000 + (i % 5_000) as i32,
            ..MnHot::default()
        },
        cold: Arc::new(cold),
    })
}

/// Core's `CDeterministicMN` serialization (`SER_DISK`) of `dmn`, the bytes the oracle's list
/// operations take (node-l1's DML differential).
pub fn dmn_bytes(dmn: &Dmn) -> Vec<u8> {
    let mut out = Vec::new();
    crate::codec::write_dmn(dmn, &mut out);
    out
}

/// Core's `CDeterministicMNList::Serialize` (`SER_DISK`, `deterministicmns.h` v24:235-244) of
/// `list`, the evodb `dmn_S3` encoding: the canonical list's header (block hash, height,
/// registration counter), then every masternode in Core's iteration order
/// ([`crate::MnList::core_order`]), so it covers the internal ids, the counter and the collision
/// buckets' order (node-l1's DML differential).
///
/// # Panics
///
/// Never: every proTxHash `core_order` returns is in the list.
pub fn list_disk_bytes(list: &crate::MnList) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(list.block_hash().as_bytes());
    out.extend_from_slice(&list.height().to_le_bytes());
    out.extend_from_slice(&list.total_registered().to_le_bytes());
    dash_consensus::primitives::write_compact_size(list.len() as u64, &mut out);
    for pro_tx in list.core_order().iter() {
        crate::codec::write_dmn(list.get(pro_tx).expect("a member"), &mut out);
    }
    out
}

/// A quorum member as evodb stores it (`CDeterministicMN`, `SER_DISK`) with `pro_tx_hash` and
/// [`dmn`]`(0)`'s other fields: the build and Core's `HandleQuorumCommitment` read only a
/// member's proTxHash (EV2b D32).
pub fn member_bytes(pro_tx_hash: crate::ProTxHash) -> Vec<u8> {
    dmn_bytes(&Dmn {
        pro_tx_hash,
        ..Dmn::clone(&dmn(0, MnType::Regular))
    })
}

/// Core's `CDeterministicMNState` serialization (`SER_DISK`) of `dmn`'s state.
pub fn state_bytes(dmn: &Dmn) -> Vec<u8> {
    crate::codec::state_bytes(dmn)
}

/// Core's `CDeterministicMNStateDiff` serialization of `diff`.
pub fn state_diff_bytes(diff: &crate::MnStateDiff) -> Vec<u8> {
    let mut out = Vec::new();
    crate::codec::write_state_diff(diff, &mut out);
    out
}

/// Core's vector serialization of `bytes`: its compact size, then the bytes.
fn vec_wire(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    dash_consensus::primitives::write_compact_size(bytes.len() as u64, &mut out);
    out.extend_from_slice(bytes);
    out
}

/// A ProTx payload whose signature, its last encoded field, base-sdk keeps private: it is set by
/// `Signable::set_signature`, which verifies it, or `reset_signature`. A test that stores a given
/// signature (any bytes, or one made for another key or scheme) uses [`WithSig::with_sig`]. The
/// repin port (DEC-1055) also rebuilds signed payloads through `new`, which resets the signature,
/// and puts the old one back with it.
pub trait WithSig: BaseCodec + Clone + PartialEq + std::fmt::Debug + Sized {
    type Sig: PartialEq + std::fmt::Debug;
    fn sig_field(&self) -> Self::Sig;
    /// `sig` as this payload's encoding writes it.
    fn sig_wire(&self, sig: &Self::Sig) -> Vec<u8>;

    /// The payload's encoding up to its signature, its last field.
    fn unsigned_wire(&self) -> Vec<u8>;

    /// The payload with `sig` in place of its signature: the payload itself when it holds `sig`
    /// already, else its encoding with the signature's bytes replaced, decoded. Asserts the
    /// payload but its signature round-trips, by its `Debug` form, and the signature decodes as
    /// `sig`, so the result equals the payload with only the signature changed.
    fn with_sig(&self, sig: Self::Sig) -> Self {
        if self.sig_field() == sig {
            return self.clone();
        }
        let unsigned = self.unsigned_wire();
        let wire = [&unsigned[..], &self.sig_wire(&self.sig_field())].concat();
        let mut cursor = &wire[..];
        let back = Self::decode(&mut cursor).expect("the payload decodes");
        assert!(cursor.is_empty(), "the payload is its unsigned bytes and its signature");
        // The signature, the last field, is left out: it is replaced.
        let but_sig = |d: String| d.rfind(", sig: ").map_or(d.clone(), |i| d[..i].to_owned());
        assert_eq!(
            but_sig(format!("{back:?}")),
            but_sig(format!("{self:?}")),
            "the payload round-trips, its signature aside"
        );
        let wire = [&unsigned[..], &self.sig_wire(&sig)].concat();
        let mut cursor = &wire[..];
        let out = Self::decode(&mut cursor).expect("the payload decodes with the signature");
        assert!(cursor.is_empty(), "the signature is the last field");
        assert_eq!(out.sig_field(), sig, "the signature decodes as set");
        out
    }
}

impl WithSig for dash_primitives::ProRegTx {
    type Sig = dash_primitives::CollateralSig;
    fn sig_field(&self) -> Self::Sig {
        self.sig().clone()
    }
    fn unsigned_wire(&self) -> Vec<u8> {
        dash_consensus::primitives::ProRegTxUnsigned {
            payload: self,
            operator_key: *self.pub_key_operator().as_bytes().as_bytes(),
        }
        .bytes()
    }
    fn sig_wire(&self, sig: &Self::Sig) -> Vec<u8> {
        vec_wire(sig.as_slice())
    }
}

impl WithSig for dash_primitives::ProUpRegTx {
    type Sig = dash_primitives::OwnerSig;
    fn sig_field(&self) -> Self::Sig {
        self.sig().clone()
    }
    fn unsigned_wire(&self) -> Vec<u8> {
        dash_consensus::primitives::ProUpRegTxUnsigned {
            payload: self,
            operator_key: *self.pub_key_operator().as_bytes().as_bytes(),
        }
        .bytes()
    }
    fn sig_wire(&self, sig: &Self::Sig) -> Vec<u8> {
        vec_wire(sig.as_slice())
    }
}

macro_rules! with_operator_sig {
    ($($t:ty => $unsigned:ident),*) => {
        $(impl WithSig for $t {
            type Sig = dash_primitives::OperatorSig;
            fn sig_field(&self) -> Self::Sig {
                self.sig()
            }
            fn unsigned_wire(&self) -> Vec<u8> {
                $unsigned(self)
            }
            fn sig_wire(&self, sig: &Self::Sig) -> Vec<u8> {
                sig.as_bytes().as_bytes().to_vec()
            }
        }

        impl WithOperatorSig for $t {
            fn eager_sig(&self) -> BlsEagerSig {
                dash_consensus::primitives::pro_tx_operator_sig(&self.sig(), self.version())
                    .expect("a signature Core's payload read accepts")
            }

            fn with_operator_sig(&self, sig: BlsEagerSig) -> Self {
                let scheme = dash_consensus::primitives::protx_key_scheme(self.version());
                let bytes = sig.to_bytes(scheme);
                let sig = dash_primitives::OperatorSig::decode(&mut &bytes[..]).expect("96 bytes");
                self.with_sig(sig)
            }
        })*
    };
}

/// base-sdk's ProUpServTx encoding up to the signature (`provider/proupservtx.rs` `encode`).
fn serv_unsigned(p: &dash_primitives::ProUpServTx) -> Vec<u8> {
    use dash_consensus::primitives::{write_bytes, write_net_info};
    use dash_primitives::{MnType, ProTxVersion};
    let (mut w, v24) = (Vec::new(), p.version() >= ProTxVersion::CoreV24);
    w.write(&u16::from(p.version()).to_le_bytes());
    if p.version() >= ProTxVersion::BasicBls {
        w.write(&p.mn_type().to_base().to_le_bytes());
    }
    w.write(p.pro_tx_hash().as_bytes());
    write_net_info(p.net_info(), v24, &mut w);
    write_bytes(p.script_operator_payout().as_bytes(), &mut w);
    w.write(p.inputs_hash().as_bytes());
    if p.mn_type() == MnType::Evo {
        w.write(p.platform_node_id().unwrap_or_default().as_bytes());
        if !v24 {
            w.write(&p.platform_p2p_port().unwrap_or(0).to_le_bytes());
            w.write(&p.platform_http_port().unwrap_or(0).to_le_bytes());
        }
    }
    w
}

/// base-sdk's ProUpRevTx encoding up to the signature (`provider/prouprevtx.rs` `encode`).
fn rev_unsigned(p: &dash_primitives::ProUpRevTx) -> Vec<u8> {
    let mut w = Vec::new();
    w.write(&u16::from(p.version()).to_le_bytes());
    w.write(p.pro_tx_hash().as_bytes());
    w.write(&p.reason().to_base().to_le_bytes());
    w.write(p.inputs_hash().as_bytes());
    w
}

/// A ProUpServTx or ProUpRevTx given an eager operator signature, written as the payload's
/// encoding wrote one before the re-pin: its object under the version's scheme.
pub trait WithOperatorSig: WithSig {
    /// The side-car signature a decode of the payload builds.
    fn eager_sig(&self) -> BlsEagerSig;
    fn with_operator_sig(&self, sig: BlsEagerSig) -> Self;
}

with_operator_sig!(
    dash_primitives::ProUpServTx => serv_unsigned,
    dash_primitives::ProUpRevTx => rev_unsigned
);

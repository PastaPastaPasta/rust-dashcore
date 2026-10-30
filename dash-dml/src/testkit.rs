//! Test builders (feature `test-utils`), for this crate's tests and its consumers' (EV2 §2.1
//! `testkit`): simple masternodes, plus `dash_evo`'s block tree and proTxHashes. EV2a adds its
//! list builders and a synthetic block generator here. Lists are filled with
//! `MnList::insert_for_tests`.

use std::sync::Arc;

use dash_pkc::bls::{BlsLazyPk, BlsScIetf, BlsSchemeKind, BlsSchemeSource, BlsSecretKey};
use dash_primitives::{NetInfo, NetInfoV1, OutPoint, PlatformNodeId, TxHash};
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
        collateral: OutPoint {
            hash: TxHash::from([n; 32]),
            index: 0,
        },
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

/// [`operator_key`] of the secret derived from `ikm`.
///
/// # Panics
///
/// Never: 32 bytes are enough key material.
pub fn operator_key_from(ikm: &[u8; 32], scheme: BlsSchemeKind) -> OperatorKey {
    BlsLazyPk::from_value(basic_pk(ikm).get(), scheme)
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
        net_info: Arc::new(NetInfo::Legacy(NetInfoV1(ServiceV1 {
            addr: AddrV1(ip),
            port: 9999,
        }))),
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
        collateral: OutPoint {
            hash: TxHash::from(spread(i, 9)),
            index: i % 3,
        },
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
